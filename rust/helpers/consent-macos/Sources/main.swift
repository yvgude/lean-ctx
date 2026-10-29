// SPDX-License-Identifier: Apache-2.0
// LeanCTX Consent.app (SPEC-ENGINE §3.8.4, §3.8.5): holds this Mac's presence
// key in the Secure Enclave and signs consent proofs only after the person
// confirms the request in the system dialog.
//
// - Started by lean-ctx with `--socket <path>`; a 32-byte session secret
//   (64 hex) arrives on stdin. One client, which must present the secret and
//   pass the code-signature peer check (same Team ID as this helper).
// - JSON-RPC 2.0, one object per line: `presence`, `bind_key`, `public_key`.
// - The dialog text comes only from the fixed per-kind templates below,
//   filled from the subject that is signed; there is no text parameter.

import CryptoKit
import Darwin
import Foundation
import LocalAuthentication
import Security

let presenceLabel = "ch.thinkery.leanctx.presence"
let proofDomain = "leanctx.hive.consent_proof.v1\n"
let bindDomain = "leanctx.hive.presence_bind.v1\n"

/// The subject fields per kind (C0 `enums/consent-kinds.json`);
/// `procedure_confirm` is web only and never signed here.
let subjectFields: [String: [String]] = [
    "link_vault": ["bound_repo", "link_digest", "remote_seen", "target_vault", "vault"],
    "public_presence": ["vault", "worker_id"],
    "swarm_join": ["cli", "pledge_digest", "swarm"],
    "pledge_change": ["after_digest", "before_digest", "swarm"],
    "unattended_start": ["cli", "pledge_digest", "swarm", "usd_cents_per_day"],
    "reviewer_enable": ["cli", "place", "reviewer_usd_cents_per_day", "source"],
    "api_key_set": ["cli", "vendor"],
    "sealed_grant": ["engine_fingerprint", "requester", "vault"],
]

struct RPCError: Error {
    let code: Int
    let message: String
}

func fenced(_ value: JSONValue?) -> String {
    // Agent-reachable text is shown quoted and clipped, never as prose.
    let text = value?.string ?? value?.canonical() ?? "?"
    let clipped = text.count > 80 ? String(text.prefix(79)) + "…" : text
    return "“" + clipped.replacingOccurrences(of: "\n", with: " ") + "”"
}

func dollars(_ value: JSONValue?) -> String {
    guard case .uint(let cents) = value else { return "?" }
    return String(format: "USD %.2f", Double(cents) / 100)
}

/// The fixed dialog text for a subject (the same questions as WEB §3.11).
func template(kind: String, subject: JSONValue, digest: String) -> String? {
    let s = { (k: String) in fenced(subject[k]) }
    let question: String
    switch kind {
    case "link_vault":
        question = "Use vault \(s("vault")) for the repository \(s("remote_seen"))?"
    case "public_presence":
        question = "Show worker \(s("worker_id")) on the public bus of vault \(s("vault"))?"
    case "swarm_join":
        question = "Let \(s("cli")) on this Mac join swarm \(s("swarm"))?"
    case "pledge_change":
        question = "Raise your pledge to swarm \(s("swarm"))? Lowering a limit never needs approval."
    case "unattended_start":
        question = "Let swarm \(s("swarm")) run unattended with \(s("cli")), up to \(dollars(subject["usd_cents_per_day"])) per day?"
    case "reviewer_enable":
        question = "Run reviews for \(s("place")) with \(s("cli")), up to \(dollars(subject["reviewer_usd_cents_per_day"])) per day (\(s("source")) capacity)?"
    case "api_key_set":
        question = "Store an API key for \(s("cli")) (\(s("vendor"))) on this Mac?"
    case "sealed_grant":
        question = "Give engine \(s("engine_fingerprint")) of \(s("requester")) access to sealed vault \(s("vault"))?"
    default:
        return nil
    }
    return "LeanCTX: \(question) Request fingerprint: \(digest.prefix(16))."
}

// MARK: - Presence key

enum PresenceKey {
    static func find(context: LAContext?) -> SecKey? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrLabel as String: presenceLabel,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecReturnRef as String: true,
        ]
        if let context { query[kSecUseAuthenticationContext as String] = context }
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let item else { return nil }
        return (item as! SecKey)
    }

    /// Secure Enclave first; without one, a login-keychain key that still
    /// needs the dialog for every use (`hardware = false`).
    static func create() throws -> (SecKey, Bool) {
        for enclave in [true, false] {
            var error: Unmanaged<CFError>?
            let flags: SecAccessControlCreateFlags = enclave ? [.privateKeyUsage, .userPresence] : [.userPresence]
            guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, flags, &error) else { continue }
            var attributes: [String: Any] = [
                kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
                kSecAttrKeySizeInBits as String: 256,
                kSecAttrLabel as String: presenceLabel,
                kSecPrivateKeyAttrs as String: [
                    kSecAttrIsPermanent as String: true,
                    kSecAttrAccessControl as String: access,
                ],
            ]
            if enclave { attributes[kSecAttrTokenID as String] = kSecAttrTokenIDSecureEnclave }
            if let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) { return (key, enclave) }
        }
        throw RPCError(code: -32000, message: "presence key could not be created")
    }

    static func hardware(_ key: SecKey) -> Bool {
        let attributes = SecKeyCopyAttributes(key) as? [String: Any]
        return (attributes?[kSecAttrTokenID as String] as? String) == (kSecAttrTokenIDSecureEnclave as String)
    }

    /// SubjectPublicKeyInfo DER of the P-256 public key.
    static func spki(_ key: SecKey) throws -> Data {
        guard let pub = SecKeyCopyPublicKey(key),
              let raw = SecKeyCopyExternalRepresentation(pub, nil) as Data? else {
            throw RPCError(code: -32000, message: "public key unavailable")
        }
        let prefix: [UInt8] = [
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
            0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
        ]
        return Data(prefix) + raw
    }

    static func keyID(_ spki: Data) -> String {
        SHA256.hash(data: spki).map { String(format: "%02x", $0) }.joined().prefix(16).description
    }

    static func sign(_ key: SecKey, _ message: Data) throws -> Data {
        var error: Unmanaged<CFError>?
        guard let signature = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256, message as CFData, &error) as Data? else {
            throw RPCError(code: 1, message: "cancelled")
        }
        return signature
    }
}

func b64url(_ data: Data) -> String {
    data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}

// MARK: - Methods

func presence(_ params: JSONValue) throws -> JSONValue {
    guard case .object(let p) = params, Set(p.keys) == ["consent_id", "kind", "subject"],
          let consentID = p["consent_id"]?.string, let kind = p["kind"]?.string,
          let subject = p["subject"], case .object(let fields) = subject,
          let expected = subjectFields[kind], Set(fields.keys) == Set(expected),
          consentID.hasPrefix("cns_"), consentID.count == 20
    else { throw RPCError(code: -32602, message: "invalid_params") }
    let digest = Blake3.hex(subject.canonical())
    guard let reason = template(kind: kind, subject: subject, digest: digest) else {
        throw RPCError(code: -32602, message: "invalid_params")
    }
    let context = LAContext()
    context.localizedReason = reason
    context.touchIDAuthenticationAllowableReuseDuration = 0
    guard let key = PresenceKey.find(context: context) else {
        throw RPCError(code: -32001, message: "no presence key; run lean-ctx hive connect --bind-presence-key")
    }
    let approvedAt = UInt64(Date().timeIntervalSince1970 * 1000)
    let payload = JSONValue.object([
        "consent_id": .string(consentID), "kind": .string(kind),
        "subject_digest": .string(digest), "approved_at": .uint(approvedAt),
    ])
    let signature = try PresenceKey.sign(key, Data((proofDomain + payload.canonical()).utf8))
    let spki = try PresenceKey.spki(key)
    return .object([
        "approved_at": .uint(approvedAt),
        "key_id": .string(PresenceKey.keyID(spki)),
        "signature": .string(b64url(signature)),
    ])
}

func publicKey() throws -> JSONValue {
    guard let key = PresenceKey.find(context: nil) else {
        throw RPCError(code: -32001, message: "no presence key")
    }
    let spki = try PresenceKey.spki(key)
    return .object([
        "alg": .string("p256_ecdsa_sha256"), "public_key": .string(b64url(spki)),
        "key_id": .string(PresenceKey.keyID(spki)), "hardware": .bool(PresenceKey.hardware(key)),
    ])
}

func bindKey(_ params: JSONValue) throws -> JSONValue {
    guard case .object(let p) = params, Set(p.keys) == ["instance_id"], let instance = p["instance_id"]?.string else {
        throw RPCError(code: -32602, message: "invalid_params")
    }
    let key = try PresenceKey.find(context: nil) ?? PresenceKey.create().0
    let spki = try PresenceKey.spki(key)
    let statement = JSONValue.object([
        "instance_id": .string(instance), "key_id": .string(PresenceKey.keyID(spki)),
        "alg": .string("p256_ecdsa_sha256"), "public_key": .string(b64url(spki)),
    ])
    let context = LAContext()
    context.localizedReason = "LeanCTX: bind this Mac's system confirmation key (\(PresenceKey.keyID(spki))) to your Hive instance?"
    let signing = PresenceKey.find(context: context) ?? key
    let signature = try PresenceKey.sign(signing, Data((bindDomain + statement.canonical()).utf8))
    return .object([
        "alg": .string("p256_ecdsa_sha256"), "public_key": .string(b64url(spki)),
        "key_id": .string(PresenceKey.keyID(spki)), "hardware": .bool(PresenceKey.hardware(key)),
        "card_bind_signature": .string(b64url(signature)),
    ])
}

// MARK: - Peer check and IPC

/// The Team ID this helper is signed with; nil when unsigned (development).
func ownTeamID() -> String? {
    var code: SecCode?
    guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return nil }
    var staticCode: SecStaticCode?
    guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return nil }
    var info: CFDictionary?
    guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess
    else { return nil }
    return (info as? [String: Any])?[kSecCodeInfoTeamIdentifier as String] as? String
}

/// The connecting process must carry a valid Developer ID signature with
/// this helper's Team ID (LOCAL_PEERTOKEN audit token → SecCode).
func peerAllowed(_ fd: Int32, team: String) -> Bool {
    var token = audit_token_t()
    var length = socklen_t(MemoryLayout<audit_token_t>.size)
    guard getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN, &token, &length) == 0 else { return false }
    let tokenData = withUnsafeBytes(of: &token) { Data($0) }
    var guest: SecCode?
    let attributes = [kSecGuestAttributeAudit: tokenData] as CFDictionary
    guard SecCodeCopyGuestWithAttributes(nil, attributes, [], &guest) == errSecSuccess, let guest else { return false }
    var requirement: SecRequirement?
    let text = "anchor apple generic and certificate leaf[subject.OU] = \"\(team)\"" as CFString
    guard SecRequirementCreateWithString(text, [], &requirement) == errSecSuccess, let requirement else { return false }
    return SecCodeCheckValidity(guest, [], requirement) == errSecSuccess
}

func respond(_ handle: FileHandle, id: JSONValue, result: Result<JSONValue, RPCError>) {
    var object: [String: JSONValue] = ["jsonrpc": .string("2.0"), "id": id]
    switch result {
    case .success(let value): object["result"] = value
    case .failure(let error):
        object["error"] = .object(["code": .int(Int64(error.code)), "message": .string(error.message)])
    }
    handle.write(Data((JSONValue.object(object).canonical() + "\n").utf8))
}

func serve(socketPath: String, secret: String) -> Int32 {
    guard let team = ownTeamID() else {
        FileHandle.standardError.write(Data("helper is not signed with a Team ID\n".utf8))
        return 3
    }
    unlink(socketPath)
    let server = socket(AF_UNIX, SOCK_STREAM, 0)
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    let pathBytes = Array(socketPath.utf8)
    guard pathBytes.count < MemoryLayout.size(ofValue: address.sun_path) else { return 2 }
    withUnsafeMutableBytes(of: &address.sun_path) { buffer in
        buffer.copyBytes(from: pathBytes + [0])
    }
    let previous = umask(0o177)
    let bound = withUnsafePointer(to: &address) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(server, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
    }
    umask(previous)
    guard bound == 0, listen(server, 1) == 0 else { return 2 }
    // One client only; the socket is gone once it connected.
    let client = accept(server, nil, nil)
    close(server)
    unlink(socketPath)
    guard client >= 0, peerAllowed(client, team: team) else { return 4 }
    let handle = FileHandle(fileDescriptor: client, closeOnDealloc: true)
    var buffer = Data()
    var authenticated = false
    while true {
        let chunk = handle.availableData
        if chunk.isEmpty { return 0 }
        buffer += chunk
        while let newline = buffer.firstIndex(of: 0x0A) {
            let line = buffer[buffer.startIndex..<newline]
            buffer = buffer[(newline + 1)...]
            guard let request = try? JSONValue.parse(Data(line)) else { return 5 }
            let id = request["id"] ?? .null
            if !authenticated {
                // The first request proves the session secret.
                guard request["method"]?.string == "hello",
                      let given = request["params"]?["secret"]?.string,
                      given.utf8.count == secret.utf8.count,
                      zip(given.utf8, secret.utf8).reduce(0, { $0 | ($1.0 ^ $1.1) }) == 0
                else { return 6 }
                authenticated = true
                respond(handle, id: id, result: .success(.object(["ok": .bool(true)])))
                continue
            }
            let params = request["params"] ?? .object([:])
            let result: Result<JSONValue, RPCError>
            do {
                switch request["method"]?.string {
                case "presence": result = .success(try presence(params))
                case "bind_key": result = .success(try bindKey(params))
                case "public_key": result = .success(try publicKey())
                default: result = .failure(RPCError(code: -32601, message: "method not found"))
                }
            } catch let error as RPCError {
                result = .failure(error)
            } catch {
                result = .failure(RPCError(code: -32000, message: "\(error)"))
            }
            respond(handle, id: id, result: result)
        }
    }
}

/// `--selftest <consent_subject.vectors.json>`: canonical JSON and BLAKE3
/// against the C0 vectors (run in CI and before signing).
func selftest(_ path: String) -> Int32 {
    guard let data = FileManager.default.contents(atPath: path),
          let vectors = try? JSONValue.parse(data), case .array(let cases)? = vectors["cases"] else { return 2 }
    var failed = 0
    for item in cases {
        guard let subject = item["subject"] else { failed += 1; continue }
        let canonical = subject.canonical()
        let digest = Blake3.hex(canonical)
        if canonical != item["canonical"]?.string || digest != item["subject_digest"]?.string {
            failed += 1
            FileHandle.standardError.write(Data("mismatch: \(item["kind"]?.string ?? "?")\n".utf8))
        }
        let kind = item["kind"]?.string ?? ""
        if kind != "procedure_confirm" && template(kind: kind, subject: subject, digest: digest) == nil { failed += 1 }
    }
    let empty = Blake3.hex("")
    if empty != "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262" { failed += 1 }
    // Multi-chunk path, pinned against the reference crate in the engine's
    // consent tests.
    let long = Blake3.hex(String(repeating: "a", count: 5000))
    if long != "09d0d29a5f2dc69dff0809823ca867836c3a3cfb00e12df06d92e3f0f70629e9" { failed += 1 }
    print("selftest: \(cases.count) cases, \(failed) failed")
    return failed == 0 ? 0 : 1
}

let args = CommandLine.arguments
if args.count == 3, args[1] == "--selftest" {
    exit(selftest(args[2]))
}
guard args.count == 3, args[1] == "--socket" else {
    FileHandle.standardError.write(Data("usage: LeanCTX Consent --socket <path>\n".utf8))
    exit(2)
}
guard let line = readLine(strippingNewline: true), line.count == 64, line.allSatisfy(\.isHexDigit) else {
    exit(6)
}
exit(serve(socketPath: args[2], secret: line))
