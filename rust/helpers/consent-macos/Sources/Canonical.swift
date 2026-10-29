// SPDX-License-Identifier: Apache-2.0
// Canonical JSON and BLAKE3 for LeanCTX Consent.app (SPEC-ENGINE §3.8.5):
// the helper recomputes subject_digest itself and builds the signed payload
// from the canonical subject, so no caller-supplied bytes are ever signed.

import Foundation

enum JSONValue: Equatable {
    case null
    case bool(Bool)
    case int(Int64)
    case uint(UInt64)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    struct NotCanonical: Error {}

    /// Parse with JSONSerialization; non-integer numbers are refused, as in
    /// `lean_ctx_protocol::hive::canonical_json`.
    static func parse(_ data: Data) throws -> JSONValue {
        let any = try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        return try from(any)
    }

    static func from(_ any: Any) throws -> JSONValue {
        switch any {
        case is NSNull:
            return .null
        case let n as NSNumber:
            if CFGetTypeID(n) == CFBooleanGetTypeID() { return .bool(n.boolValue) }
            if CFNumberIsFloatType(n) { throw NotCanonical() }
            if n.int64Value < 0 { return .int(n.int64Value) }
            return .uint(n.uint64Value)
        case let s as String:
            return .string(s)
        case let a as [Any]:
            return .array(try a.map(from))
        case let o as [String: Any]:
            var out: [String: JSONValue] = [:]
            for (k, v) in o { out[k] = try from(v) }
            return .object(out)
        default:
            throw NotCanonical()
        }
    }

    var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    /// Keys sorted by their UTF-8 bytes, no whitespace, serde_json escaping.
    func canonical() -> String {
        switch self {
        case .null: return "null"
        case .bool(let b): return b ? "true" : "false"
        case .int(let i): return String(i)
        case .uint(let u): return String(u)
        case .string(let s): return JSONValue.quote(s)
        case .array(let a): return "[" + a.map { $0.canonical() }.joined(separator: ",") + "]"
        case .object(let o):
            let keys = o.keys.sorted { Array($0.utf8).lexicographicallyPrecedes(Array($1.utf8)) }
            return "{" + keys.map { JSONValue.quote($0) + ":" + o[$0]!.canonical() }.joined(separator: ",") + "}"
        }
    }

    static func quote(_ s: String) -> String {
        var out = "\""
        for scalar in s.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\u{08}": out += "\\b"
            case "\u{0C}": out += "\\f"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case let c where c.value < 0x20:
                out += String(format: "\\u%04x", c.value)
            default:
                out.unicodeScalars.append(scalar)
            }
        }
        return out + "\""
    }
}

/// BLAKE3 (default hash mode, 32-byte output), single-threaded.
enum Blake3 {
    private static let iv: [UInt32] = [
        0x6A09_E667, 0xBB67_AE85, 0x3C6E_F372, 0xA54F_F53A,
        0x510E_527F, 0x9B05_688C, 0x1F83_D9AB, 0x5BE0_CD19,
    ]
    private static let perm: [Int] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
    private static let chunkStart: UInt32 = 1, chunkEnd: UInt32 = 2, parent: UInt32 = 4, root: UInt32 = 8

    private static func g(_ s: inout [UInt32], _ a: Int, _ b: Int, _ c: Int, _ d: Int, _ x: UInt32, _ y: UInt32) {
        s[a] = s[a] &+ s[b] &+ x; s[d] = (s[d] ^ s[a]).rotr(16)
        s[c] = s[c] &+ s[d]; s[b] = (s[b] ^ s[c]).rotr(12)
        s[a] = s[a] &+ s[b] &+ y; s[d] = (s[d] ^ s[a]).rotr(8)
        s[c] = s[c] &+ s[d]; s[b] = (s[b] ^ s[c]).rotr(7)
    }

    private static func compress(_ cv: [UInt32], _ block: [UInt32], _ counter: UInt64, _ len: UInt32, _ flags: UInt32) -> [UInt32] {
        var s = cv + Array(iv[0..<4]) + [UInt32(truncatingIfNeeded: counter), UInt32(truncatingIfNeeded: counter >> 32), len, flags]
        var m = block
        for round in 0..<7 {
            g(&s, 0, 4, 8, 12, m[0], m[1]); g(&s, 1, 5, 9, 13, m[2], m[3])
            g(&s, 2, 6, 10, 14, m[4], m[5]); g(&s, 3, 7, 11, 15, m[6], m[7])
            g(&s, 0, 5, 10, 15, m[8], m[9]); g(&s, 1, 6, 11, 12, m[10], m[11])
            g(&s, 2, 7, 8, 13, m[12], m[13]); g(&s, 3, 4, 9, 14, m[14], m[15])
            if round < 6 { m = perm.map { m[$0] } }
        }
        for i in 0..<8 { s[i] ^= s[i + 8]; s[i + 8] ^= cv[i] }
        return s
    }

    private static func words(_ bytes: ArraySlice<UInt8>) -> [UInt32] {
        var block = [UInt8](bytes); block += [UInt8](repeating: 0, count: 64 - block.count)
        return (0..<16).map { i in
            UInt32(block[4 * i]) | UInt32(block[4 * i + 1]) << 8 | UInt32(block[4 * i + 2]) << 16 | UInt32(block[4 * i + 3]) << 24
        }
    }

    /// The chaining value (or, with `isRoot`, the root output) of one chunk.
    private static func chunk(_ data: ArraySlice<UInt8>, _ counter: UInt64, isRoot: Bool) -> [UInt32] {
        var cv = iv
        let blocks = max(1, (data.count + 63) / 64)
        for b in 0..<blocks {
            let start = data.startIndex + b * 64
            let block = data[start..<min(start + 64, data.endIndex)]
            var flags: UInt32 = 0
            if b == 0 { flags |= chunkStart }
            if b == blocks - 1 { flags |= chunkEnd | (isRoot ? root : 0) }
            cv = Array(compress(cv, words(block), counter, UInt32(block.count), flags)[0..<8])
        }
        return cv
    }

    private static func parentCV(_ l: [UInt32], _ r: [UInt32], isRoot: Bool) -> [UInt32] {
        Array(compress(iv, l + r, 0, 64, parent | (isRoot ? root : 0))[0..<8])
    }

    static func hash(_ input: [UInt8]) -> [UInt8] {
        let chunks = max(1, (input.count + 1023) / 1024)
        var out: [UInt32]
        if chunks == 1 {
            out = chunk(input[...], 0, isRoot: true)
        } else {
            // Merge subtrees as in the reference: a stack of chaining values.
            var stack: [[UInt32]] = []
            for c in 0..<chunks {
                let start = c * 1024
                var cv = chunk(input[start..<min(start + 1024, input.count)], UInt64(c), isRoot: false)
                var total = UInt64(c + 1)
                while total & 1 == 0 && c < chunks - 1 {
                    cv = parentCV(stack.removeLast(), cv, isRoot: false)
                    total >>= 1
                }
                stack.append(cv)
            }
            while stack.count > 1 {
                let r = stack.removeLast(), l = stack.removeLast()
                stack.append(stack.isEmpty ? parentCV(l, r, isRoot: true) : parentCV(l, r, isRoot: false))
            }
            out = stack[0]
        }
        return out.flatMap { w in (0..<4).map { UInt8(truncatingIfNeeded: w >> (8 * $0)) } }
    }

    static func hex(_ input: String) -> String {
        hash(Array(input.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

extension UInt32 {
    func rotr(_ n: UInt32) -> UInt32 { (self >> n) | (self << (32 - n)) }
}
