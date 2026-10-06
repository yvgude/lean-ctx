# Optional Intelligence Runtime installation

The public host owns package verification, installation and context admission.
The private runtime does not install itself or migrate user context. Its absence
leaves the public reference engine available. New commercial builds also enforce
their own paid-feature entitlement; this does not grant access to context sources.

Authenticated package metadata must explicitly declare `entitlement_required`
as a boolean. Historical `false` packages retain their installation semantics.
For `true`, invocation requires `LEANCTX_INTELLIGENCE_LICENSE_CONFIG`, an absolute
path to bounded regular operator-provisioned configuration. The host passes only
this explicit setting through its cleared child environment; the private peer
owns the signed entitlement interpretation and rejects unpaid operations.
Description/health and retained public data do not require a paid invocation.
No missing/expired license authorizes bypass of context protection.

The production installation profile below is distinct from the explicit staging
operator commands. Synthetic authority used in qualification is not a rollout or
publication grant.

## Normal Pro installation

A trusted host build may carry a production channel using the three build inputs
`LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL`,
`LEANCTX_PRODUCTION_RUNTIME_CHANNEL_SIGNATURE_URL` and
`LEANCTX_PRODUCTION_RUNTIME_CHANNEL_ROOT_KEY_HEX`. They are compiled into the
host; runtime environment variables, project files and a downloaded key cannot
replace that root. Missing inputs disable production discovery; partial or
invalid inputs fail the host build.

The release workflow reads repository Actions variables with these same three
names after its existing release/provenance gates. Set all three to the selected
customer catalog URL, detached signature URL and public trust root before tagging
a Pro-capable host. The workflow exports only a complete, bounded HTTPS tuple to
Cargo; unset variables preserve a Community build without production discovery.
No private signing key belongs in these variables. This wiring does not create
catalogs, publish packages, authorize release, or qualify a native platform.

With that host, `lean-ctx setup runtime status` discovers the optional runtime
without network access or starting private code. Installation requires explicit
consent: `lean-ctx setup runtime sync-configured --accept-proprietary`.
Interactive `lean-ctx setup runtime configure` uses the same authority.

Production packages use their own `intelligence-runtime-production` store.
The production catalog has schema `leanctx.runtime-channel/v2`, channel
`production`, and signature domain `leanctx-runtime-channel-v2\0`.
Each installed package retains the original signed catalog and signer identity.
Every use rechecks its exact host target, manifest digest and delegated artifact
key against the compiled root and authenticated rotation chain. The key in local
configuration is a consistency check, never production trust authority.
Staging packages and arbitrary operator-supplied keys cannot enter this path.

New installation/update requires a currently valid catalog and monotonic sequence.
An already installed package retains its signature authority after the catalog's
expiry; signed entitlement expiry separately stops paid operations. Production
root-transition documents use schema/domain v2 and channel `production`;
retained catalogs must fall within their signing root's sequence interval.
Changing a configuration flag cannot turn a staging artifact into a commercial
one. Stored content policies continue to apply if optional private execution fails.

This mechanism does not publish a channel or provision real signing keys. Native
platform, migration and external-user qualification remain explicit release gates.

## Personal project sync setup

On macOS and Linux, `lean-ctx engine runtime sync-project-enable --project <ABS_PROJECT> --staging --accept-proprietary`
uses the saved active staged license and verified private runtime to configure
the selected canonical project. The public host rejects broad or unsafe project
roots, pins its canonical executable path and SHA-256, and enables the existing
sync service only after the private setup response names the expected
`sync.json`. Repeating setup for the same canonical project reuses its private
configuration and returns its path with the service status. Use that path with
`sync-service-status` or `sync-service-remove`; removal preserves context and keys.
This grants no source rights; current Engine content admission remains mandatory.
The private runtime enforces entitlement and key custody. Setup cannot silently
replace an established key or change its pinned Engine; an upgrade/rebind flow,
second-device onboarding and native Linux qualification remain separate gates.

## Offline staging command

Artifact schema `leanctx.private-runtime-artifact/v1` retains its staging-only
license and `leanctx-release-manifest-v1\0` signature domain. Commercial schema
`leanctx.private-runtime-artifact/v2` uses `LicenseRef-Proprietary` and the distinct
`leanctx-release-manifest-v2\0` domain. The signed archive must declare an
entitlement-required runtime in both its description and source provenance,
and its description must carry a valid SHA-256 license-issuer fingerprint.
These fields describe the artifact; they do not grant a customer license or
approve publication. Build provenance retains `release_approved: false`.

The explicit verifier can inspect either schema against independently supplied
trust. Existing staging installation, rollback and execution reject v2 packages.
Commercial installation uses the separate host-pinned production channel and
persisted, verifiable delegation described above.

`lean-ctx engine runtime verify|install|rollback|health|invoke` is the explicit staging entry
point. It performs no downloads and never starts or replaces the installed CLI.
All operations require `--staging`. Package-bound operations require an independently selected
`--manifest-sha256` and an independently provisioned raw Ed25519 public key as
64 lowercase hexadecimal characters in `--trust-key-hex`. Never derive trust
from a key distributed beside an untrusted package.

Native header admission recognizes host-matching Mach-O, GNU ELF64 and Windows
PE32+ console images (x64/ARM64). The PE check bounds the DOS/COFF/optional/section
headers and rejects foreign architectures, DLLs and system images; it is not a
complete loader or a runnable-artifact proof. The signed archive member remains
`leanctx-intelligence`; local Windows materialization uses `leanctx-intelligence.exe`.
Windows private-directory, install and invocation paths remain fail-closed until
their native access-control and process integration is delivered.

Installer input reads are bounded and reject non-regular leaves before and after
opening. Windows opens the leaf without following reparse points and rejects all
reparse tags, including cloud placeholders, deduplicated and WIM-backed files;
use a local non-reparse copy before verification. This leaf protection does not authenticate ancestor directories
or deliver the still-missing Windows private-store ACL and lifecycle guarantees.

`verify` and `install` require `--archive`, `--manifest`, and `--signature` file
paths. `install` additionally requires `--accept-proprietary`, `--root` and
`--expected-active` (`none` on first install; otherwise the current manifest
digest). The root must be absolute/canonical, owned by the invoking user and
private (Unix mode 0700 or stricter). If absent, its canonical, user-owned parent
must exist and must not be group/other-writable. Use a dedicated runtime
subdirectory, never the context-data directory itself.

`rollback` requires `--accept-proprietary`, `--root`, `--expected-active`, the
previous selected manifest digest, and its independent trust key. It rechecks
the retained package signature and signed relationship to the active package.
The signed `rollback_artifact` is the SHA-256 of the previous complete archive,
matching its `artifact_sha256`; it is not the digest of the extracted executable.
An update with a different rollback archive digest is rejected. The retained
executable is still independently checked against its own digest before use.

Offline install and rollback change the package selection, not the configured
runtime binding. After either succeeds, explicitly activate the selected
manifest with `lean-ctx setup runtime activate` and the same staging, consent,
root, manifest, trust-key and expected-active flags. Until activation succeeds,
the stale private binding cannot run; existing content policies still apply.

`health` requires consent, root, expected-active and the **same** independently
selected manifest digest/key. It revalidates the retained signature, package,
selection and installed binary before executing authenticated bytes in a fresh
private temporary directory. The installation lock is released before launch.
The child receives no inherited environment or stdin, only a disposable HOME
and TMPDIR. Its `--describe` output must exactly match the signed description;
only exact package descriptions naming the retired routing capability at
version 1.0.0 or 2.0.0 are admitted. That capability identifies the released
package generation; the host never invokes it (adaptive model routing was
removed in v4). The existing
process capture authority imposes a two-second deadline and 64-KiB/4-KiB
stdout/stderr limits, with bounded process-group cleanup. Child output is never
included in errors. Success reports `healthy` and `service_running: false`.
This is not a process sandbox or a defense against hostile same-user processes.

`invoke` accepts the same staging consent/selection/trust flags as `health`, plus
`--request` naming a bounded regular JSON file in the public RuntimeRequestV1
format. This is an explicit operator diagnostic, **not production admission**.
The host revalidates the selected package, engine/capability version, sequence1,
input digest and admitted policy marker. It generates a fresh key for each
one-use peer and sends the exact invocation/session/deadline through private
stdin, never argv, environment, files or logs. Existing authenticated local IPC
correlates the response; the shared capture authority bounds process lifetime
and output, and the host rejects late responses or unsuccessful cleanup.
Success returns a peer observation, not a host-issued task receipt. The peer
does not execute providers. The command is currently Unix-only; Windows rejects
it. This diagnostic does not replace host admission. Published release channels
and Windows private execution remain unfinished integration work.

Model/provider selection (`select`, `select-configured`) was removed in v4
together with adaptive model routing. LeanCTX never chooses a model; the
gateway only enforces which models a request may reach.

Task creation uses only the admitting host's `RuntimeContext.project_root`,
not whichever unrelated session was saved most recently. MCP supplies its own
session workspace; standalone CLI admission supplies its current directory.
Session ancestry is partitioned by project and session together, matching the
existing project-bound replay scope. Legacy callers and the unbound proxy use
`unknown-project`; they do not inherit another project's identity. This is a
workspace-correlation boundary, **not** account authentication or permission to
load personal history by itself.

### Retired local routing history

Releases before v4 could record accepted/rejected outcomes per hashed
model/provider identity for adaptive routing. Nothing writes this history any
more. Existing entries remain in `autopilot history` / `autopilot export`
(`routing_history`) and are removed by `autopilot reset` for the selected scope.
An outcome request's `learn` flag is still accepted for compatibility and
reports `learning_recorded: false`.

### Operator-attested context outcome

`lean-ctx engine context-outcome --json REQUEST --host-stdin` accepts a version-1
request containing `receipt_digest`, `context_decision_digest`, `signals` and
an explicit `learn` boolean. The first digest is the original Unknown receipt;
the second is the signed planning artifact returned as `context_decision_ref`
by native MCP receipt publication. Host credentials remain on operator-owned
stdin and require the separate `allow_outcome_signing: true` grant (default off).
Receipt/planning authority alone does not grant terminal-outcome authority.

Signals use the existing OutcomeSignal shape, for example
`{"signal_type":"human_acceptance","value":{"boolean":true}}`.
They are operator attestations, not independently verified CI results. The
command rejects caller-supplied evidence/timestamps, agent-completion signals,
missing contract requirements, expired contract windows and conflicting repeats.
The task class comes from authenticated planning bytes, never from the request.
The previous receipt stays unchanged; the host signs an append-only successor
and Outcome decision using the existing evaluator, artifact store and ledger.

With `learn: false` no personal learning occurs. With `learn: true`, only
known local projects without tenant scope may record the validated protocol.
An exact retry reuses the receipt and can finish interrupted learning once;
it never adds another observation. No payment is required. This path evaluates
native context results; it does not claim provider-model quality, automatic
end-user feedback capture, account authorization or measured routing improvement.

### Local response measurements (not training labels)

The existing local `proxy_usage.json` meter retains up to 32 recent
`response_observations` per model. Older files load with an empty history.
Each schema-version-1 observation contains the upstream HTTP status, monotonic
milliseconds from dispatch (including retries) to usage observation, the
observation boundary, and optional provider-reported USD. Missing cost is
JSON `null`; local shadow prices, rate-table estimates and compression savings
never supply this field. Body-reported cost still takes precedence over headers.

Boundaries distinguish `body_end`, `provider_terminal_usage`, `stream_end` and
`stream_error`. A provider terminal usage event can describe a failed or
incomplete generation; HTTP 200 does not imply accepted output. Timing is not
called full response latency because terminal usage may precede transport EOF.
After a stream error, any observed partial usage is recorded once; cancellation
before a recordable boundary may leave no observation. Cache hits and requests
without a measured upstream response produce none.

These bounded diagnostic facts contain no prompts, response text, credentials,
URLs or user identity and remain in the existing local meter. They are **not**
sent to the private runtime: verified actor/endpoint scoping and explicit
accepted-outcome projection are still required before they can become training
history. No new global store, lock, telemetry event or quality label is added.

## Explicit setup and global configuration

`lean-ctx setup runtime` delegates package operations to the same runtime authority;
normal interactive setup also discloses the optional proprietary runtime.
It verifies a previously pinned global installation without starting private code,
then asks a separate default-no question before calling the existing activation
authority. Existing opt-in is reported without changing it. Missing, invalid or
declined optional runtime leaves public reference behavior available.

Non-interactive setup includes an `intelligence_runtime` report step and never
turns `--yes`, repair mode or project configuration into proprietary consent.
`lean-ctx setup runtime status --staging` exposes the same discovery without
launching private code or writing configuration. Explicit automation can use
`lean-ctx setup runtime activate-configured --staging --accept-proprietary`.
Both use independently provisioned global pins, never PATH discovery or a key
provided by the package. Unreadable global config is not overwritten.
`lean-ctx setup runtime configure --staging` runs only the normal setup's runtime
prompt in a terminal, without changing shell/editor/daemon configuration; a
non-terminal invocation is rejected instead of inferring consent.

These explicit staging examples retain their semantics. Normal Pro commands
omit `--staging` and use only the compiled production policy described above.
Published channels and native Windows delivery remain separate release gates;
staging artifacts are not production releases.

### Explicit signed staging download

`lean-ctx setup runtime download` takes the same consent, root, expected-active,
manifest digest and independent public-key flags as `install`. Replace the three
local input paths with `--archive-url`, `--manifest-url`, and `--signature-url`.
All URLs are validated before connecting: HTTPS, or literal loopback HTTP for
staging, with no userinfo, query or fragment. Redirects and environment proxies
are disabled. Platform TLS roots are honored; each request has a 120-second
deadline and the same bounded byte limits as offline installation.

The manifest and signature are authenticated before fetching the archive. The
existing verifier checks the archive; the existing atomic installer selects it.
Download failures neither select a package nor write configuration. Success does
not launch private code or enable it globally: use the existing explicit
`activate` command afterward. There is no implicit network access from setup,
no channel-supplied trust key, no second updater and no production release claim.

### Authenticated staging channel

`setup runtime sync --staging --accept-proprietary` accepts `--channel-url`,
`--channel-signature-url`, an independently provisioned **channel root** in
`--trust-key-hex`, `--root`, and `--expected-active`. It automatically selects
the signed entry for the native supported platform. No per-release digest needs
to be supplied manually. This explicit command still needs its independent root;
normal setup may instead use the host-bundled policy described below.

The bounded JSON contract is `leanctx.runtime-channel/v1`: `channel: "staging"`,
positive `sequence`, `expires_unix_ms` within seven days, and `releases` entries
containing `target`, `manifest_sha256`, `artifact_key_hex`, `archive_url`,
`manifest_url`, and `signature_url`. Fields and targets must be unambiguous.
Detached Ed25519 signs `leanctx-runtime-channel-v1` followed by one zero byte
and the exact JSON bytes. An authenticated root delegates each artifact key only
to its selected manifest digest; package contents never establish channel trust.

The existing installer atomically stores the channel root fingerprint, sequence,
catalog digest and expiry with selection schema 2. Schema 1 remains readable;
older hosts reject schema 2 instead of discarding its replay protection. Lower
sequences, conflicting equal sequences, expired metadata and unapproved root
changes fail closed. Identical retries do not rewrite selection. Offline install
and explicit rollback preserve the watermark. Local owner-controlled state is not
a defense against a hostile same-user process or deletion of the installation.

Success returns the selected artifact public key and channel receipt; activation
remains the separate explicit operation. Normal setup does not fetch implicitly.
A published channel, production licensing and Windows delivery remain incomplete.
Signed staging root transitions use the operator path below. The private release repository's staging producer
uses the same signing authority for artifact and channel documents, with separate
signature domains; it grants no production release approval.

### Setup from a provisioned staging channel

The global-only `[intelligence_runtime]` configuration accepts `channel_url`,
`channel_signature_url`, and `channel_root_key_hex` alongside `staging = true`
and the absolute private installation `root`. Provision this public trust key
independently, never from a package, catalog response or project configuration.
For a fresh installation leave the artifact pins empty and consent disabled.
The root's parent must already exist, be canonical and user-owned, and not be
group/other-writable; the installer creates only the final root with mode0700.

Normal interactive setup offers a default-no download/activation prompt when
this policy exists but no verified runtime is configured. `setup runtime
configure --staging` exercises the same step independently of editor setup.
Only an explicit yes causes catalog/package requests and bounded native health
execution. Non-interactive discovery never fetches or grants consent; automation
must invoke `setup runtime sync-configured --staging --accept-proprietary`.
That command also explicitly updates a configured runtime from its channel.

The authenticated catalog supplies the artifact pins, not a replacement root.
Successful health validation precedes opt-in persistence through the existing
comment-preserving global writer. The captured runtime policy is rechecked before
download, before health execution, and before saving; a changed policy aborts.
This is not a cross-process lock or an atomic transaction across config and store.
If package selection commits but activation fails, the verified package remains
recoverable: a retry snapshots the current store under its existing lock and
uses its compare-and-select installer. It never resets the replay watermark.
Mismatched installed pins yield `repair_available`, never `configured`; repair
requires a fresh consented authenticated download. Without a channel, invalid
installed pins retain the existing hard error. Package corruption is not silently
overwritten. Failure leaves the public reference path available and does not
grant consent. Deactivation preserves the provisioned channel and trust root.

### Host-bundled staging bootstrap

Release builders may independently provision these **build-time** variables:

- `LEANCTX_STAGING_RUNTIME_CHANNEL_URL`
- `LEANCTX_STAGING_RUNTIME_CHANNEL_SIGNATURE_URL`
- `LEANCTX_STAGING_RUNTIME_CHANNEL_ROOT_KEY_HEX`

All three must be present and syntactically admitted, or all three absent.
Partial policy fails the build; an ordinary public build embeds no channel.
Cargo tracks changes/removal, so a later unconfigured build clears the policy.
Only public metadata is embedded, never a signing key. The trusted host artifact
is the root-distribution boundary; runtime environment variables, project files
and downloaded responses cannot replace its embedded policy. A separately
provisioned global runtime configuration still takes precedence.

With no global runtime configuration, setup offers that channel without network
access, writes or implicit consent. Corrupt/unreadable global configuration is
an error, not permission to bootstrap. After explicit consent, the existing
installer creates missing canonical data parents with mode0700 and installs
under `intelligence-runtime` in the canonical data directory. Existing parent
ownership/permissions are checked, not changed; symlinks and writable parents
are rejected. The existing signed catalog/package/health path then persists the
channel, authenticated artifact pins and opt-in together. Retry keeps the same
selection and channel watermark. Deactivation does not erase the trust policy.

This supplies the first-install trust path for a configured staging host, not a
published public channel or production release authorization.

### Signed channel-root transition

`setup runtime rotate-root --staging --accept-proprietary` requires `--root`,
`--expected-active`, the original independently provisioned anchor in
`--trust-key-hex`, and a regular bounded `--transition` file. It performs no
download or private execution. Keep the original global/host anchor unchanged;
subsequent `sync` and `sync-configured` resolve the stored chain from that anchor.

The file contains exactly `document`, `previous_signature`, `next_signature`.
The latter two are lowercase hexadecimal Ed25519 signatures over
`leanctx-runtime-root-transition-v1`, one zero byte, and the exact UTF-8 document.
The document is at most 1024 bytes with exactly `schema` (value
`leanctx.runtime-root-transition/v1`), `channel` (`staging`),
`previous_root_key_hex`, `next_root_key_hex`, `minimum_sequence`, and
`expires_unix_ms`. Both keys must sign, be distinct, and extend the current chain.
Admission expires within seven days; the sequence floor must exceed both the
last accepted catalog sequence and the previous transition floor. Key reuse,
cycles, unknown fields and unsupported versions fail closed.

The existing store lock and atomic selection writer persist the full signed
proof, not just a replacement key. Selection schema 3 requires a nonempty chain;
schemas 1/2 remain readable, and hosts with only those installation readers reject
schema 3. Every sync rechecks
all signatures and order, then accepts only the final key and sequence floor;
the store rechecks them under its lock before committing a catalog. Previously
accepted transitions remain valid after their admission deadline. Exact latest
transition retries do not rewrite state. Offline install/rollback preserve the
chain. Package bytes, configuration and user context are not changed by rotation.

The chain is bounded to eight transitions and the selection to 16 KiB; overflow
fails instead of dropping trust history. This is an explicit staging operator
ceremony, not automatic transition discovery, a production rotation ceremony,
compromised-key recovery or proof of destroying old signing secrets. The private
release producer `scripts/sign_runtime_root_transition.py` uses the existing
owner-only signing authority for both keys. The separate offline
`release-key-rotation/v1` delivery-plan contract does not itself authorize these
runtime trust changes.

Runtime-only commands do not start the full setup wizard or modify agent installations.
`activate` takes the same consent, root, expected-active, manifest and trust
arguments as `health`. It verifies the signed installed package and executes
the bounded health check before persisting consent and independent pins in the
global `[intelligence_runtime]` configuration. This enables staging use only.

`lean-ctx setup runtime deactivate --staging` disables the runtime while
retaining pins, comments and unrelated configuration. Both writes use the
canonical global-config writer and refuse corrupt or unreadable existing files.
Missing configuration starts disabled. Neither operation changes context data.

## Outbound model policy

The proxy rechecks the final outgoing body, after alias rewrites and the
determinism guard, against `router-policy.toml` and answers HTTP 403 on a
violation. Missing `router-policy.toml` keeps its permissive default;
unreadable, malformed or unknown policy fields fail closed. A configured cost
ceiling fails closed because no verified per-request cost estimate exists.
Reasoning-budget policy checks the actual outgoing `thinking.budget_tokens`.

## Transaction and compatibility contract

The existing `leanctx.private-runtime-artifact/v1` exact-byte signature domain
is consumed with strict Ed25519 verification. Only the staging license marker
is currently accepted; it is **not a production license grant**. The signed
archive must contain exactly the seven builder members, all regular files,
within compressed, expanded and per-member bounds. No archive path is extracted.
Host architecture, exchange/frame version, explicit account/entitlement flags,
public receipt authority, source revision and Cargo lock digest are checked.

Packages are immutable directories addressed by signed manifest digest. The
host serializes writers, syncs a complete package, then atomically replaces one
`selection.json` holding active and previous receipts. It reuses the public
updater's durable atomic-write primitive. Repeating a selected install reports
`already_installed`; stale expectations fail. Failed verification never changes
the selection. An interruption before selection can leave an unselected complete
package, safely reusable on retry. An existing `.selection.json.tmp` causes a
fail-closed recovery requirement; inspect the interrupted operation before
removing that exact file. No automatic deletion of unknown state occurs.

This component does not claim that selection is a healthy running service.
Production trust/rotation, automatic transition distribution, published channels, remaining OCLA callers and
Windows ACL/native packaging remain separate
implementation work. No production key or trust store
is configured by this staging command.
# Explicit installed device provisioning

After installing and enabling a signed commercial runtime, an explicit operator
can hand a portal enrollment to that verified package:

```sh
lean-ctx engine runtime provision-configured --staging --accept-proprietary \
  --license-config /absolute/private/runtime.json \
  --enrollment /absolute/private/leanctx-device.json
```

The license configuration independently selects the account, device, issuer and
storage paths. Never obtain those trust choices from an unverified enrollment.
The installed private verifier owns signed entitlement, current online renewal,
credential and no-overwrite checks. The public host does not interpret the grant.
It executes authenticated installed bytes, bounds the process to60seconds and
does not forward its diagnostics or credentials.

Only successful provisioning saves `intelligence_runtime.license_configuration`
in user-global configuration. Subsequent matching configured invocations use that
path without an inherited environment variable. Project configuration cannot
supply it. Explicit independently pinned invocations retain the existing
`LEANCTX_INTELLIGENCE_LICENSE_CONFIG` fallback when no matching saved binding is
available. Changing the runtime root resets the saved binding with the other
selection fields; package updates at the same root retain it.

A failed or interrupted command can leave privately provisioned state. Inspect
that state before retrying; existing private credentials are never overwritten.
If global selection changes after provisioning, the command reports that the
device was provisioned but the configuration could not be saved. This command
does not generate operator configuration or install/start a renewal/sync service.

After provisioning, `lean-ctx engine runtime renew-configured --staging
--accept-proprietary` performs one bounded due-time check through the verified
installed package. It uses only the saved global binding, ignoring inherited
license configuration. The private verifier retains the six-hour renewal
schedule and signed expiry rules. `renewal_checked` means this check succeeded;
it does not mean a new lease was issued, that a paid operation is authorized, or
that a background service was started. A scheduler can repeat the command; a
failed check grants no additional access and never rewrites global configuration.
It remains an internal staging handoff, not a release authorization.

## Personal activation without manual license configuration

Once a signed commercial runtime is installed and enabled, the account holder can run:

```sh
lean-ctx engine runtime activate-personal --staging --accept-proprietary \
  --account-origin https://ACCOUNT_ORIGIN --issuer-origin https://ISSUER_ORIGIN
```

The host verifies installed bytes before prompting for email and a hidden password.
Automation may explicitly select `--credentials-stdin` with bounded JSON containing
`email` and `password`; secrets never belong in command arguments or environment
variables. Optional `--license-id UUID` resolves multiple eligible personal
licenses, and `--ca-certificate /absolute/private/ca.pem` selects a private CA.
Use independently configured HTTPS origins, never locations supplied by a download.

The private component authenticates through the existing account session, derives
the account binding from the current identity, verifies the signed device grant
and confirms online provisioning. It creates owner-only configuration beneath
`personal-license` in the installed root. Only success saves its path in the
global runtime configuration. Subsequent `renew-configured` and paid configured
operations use that saved binding without environment configuration.

An existing binding or destination is refused. Failures after the remote request
can leave a pending directory or provisioned device; retain that state and inspect
account devices before recovery. Retries never silently create another device.
`logout_confirmed` reports the best-effort session logout separately: `false`
means server-side session revocation was not confirmed, even when the independently
verified device activation succeeded. Session cookies are not persisted.
This command starts no background service, makes no purchase and remains staging
only. Windows private storage is not yet qualified and fails closed.

## Automatic renewal for an activated personal installation

```sh
lean-ctx engine runtime renewal-service-install --staging --accept-proprietary
lean-ctx engine runtime renewal-service-status --staging
lean-ctx engine runtime renewal-service-remove --staging
```

Install completes one saved-binding renewal check, then creates and registers a
user LaunchAgent on macOS or a user systemd timer/service on Linux. The manager
invokes this installed public host's cancellation-aware renewal tick each minute;
it reuses `renew-configured`, and the private verifier alone decides whether the
six-hour renewal is due. Every invocation
rechecks the selected private package. No Python renderer, hand-written license
configuration or inherited credential environment is needed. The user service
does not modify the existing proxy or daemon and requires a logged-in user
manager; it does not provision system-wide boot or Linux lingering.

The service records its own executable and canonical global configuration path.
Units are scoped by that path and never overwrite mismatching files. Interrupted
installation can resume matching units; uncertain native manager outcomes retain
the record for inspection. Status distinguishes manager registration from a
running check or successful license renewal. Deactivating the runtime makes
subsequent checks refuse; removing the service stops its schedule and retains
license credentials, leases and configuration. To move the public host to a new
path, remove and reinstall its renewal service from the new host.

SIGTERM/SIGINT stop the bounded private child group before the host exits; removing
the schedule does not leave its in-flight verifier running. A missing-service
result must be positively identified; uncertain native manager errors retain
service ownership and files. Unit/record publication is synced and does not
replace existing names.

No logs contain private child diagnostics. Maintenance runs with product
telemetry disabled; account entitlement checks remain independent. This is an
explicit staging service; native platform qualification is recorded separately
from rendering/compilation. Current Linux user-manager/XDG behavior still needs
native qualification, and Windows refuses it.

## Automatic sync for an explicitly selected project

```sh
lean-ctx engine runtime sync-service-install --sync-configuration /absolute/private/sync.json --staging --accept-proprietary
lean-ctx engine runtime sync-service-status --sync-configuration /absolute/private/sync.json --staging
lean-ctx engine runtime sync-service-remove --sync-configuration /absolute/private/sync.json --staging
```

This reuses the installed private runtime's existing personal checkpoint sync
configuration and saved personal license binding. Project selection, pinned
context-engine executable, host credentials and OS checkpoint key must already
be provisioned; these commands do not yet provide device pairing or setup UI.
Each configuration has its own owned user service. Admission completes one sync
cycle; the native schedule then invokes the public host once per minute. Every
cycle verifies the selected signed private package again and invokes its bounded
`--sync-once` authority. License renewal, authorization, encryption, policy checks,
revision conflicts and checkpoint continuation remain with existing components.

The service pins the selected context-data directory as well as the host and
global configuration. Reinstalling with a different data directory refuses until
the old service is removed. The private child receives that explicit data path,
the user's HOME and the saved license binding; unrelated inherited environment
and private output are not forwarded. Status exposes only fixed outcome labels
and a conflict flag. Failures expose only fixed categories such as unavailable
keys, entitlement, transport or continuation; arbitrary private stderr is never
forwarded. `last_recorded_cycle` is historical, not a freshness promise;
inspect native manager state and exit status as well. A failed cycle never enables
an unfiltered fallback, and preserved conflicts require explicit resolution.

Stopping drains an in-flight cycle within its four-minute process bound; the
native unit permits five minutes before forced shutdown. Ownership is removed
only after confirmed native absence and process completion. Uncertain removal
retains ownership for retry. Context, license, sync configuration and the last
cycle diagnostic remain available. This remains staging: native Linux operation
needs qualification, and Windows refuses the service.
The user manager must have OS access to the pinned Engine executable and selected
project. A successful foreground command does not establish that background
access; current native fixtures use an isolated project and executable outside
macOS-protected user folders. Protected-folder onboarding remains unqualified.
