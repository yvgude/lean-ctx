use super::{ReadOutput, count_tokens, format_anchored_output_window};

#[derive(Debug)]
pub(crate) struct RootedRead {
    pub(crate) content: String,
    pub(crate) canonical_path: String,
}

/// A rooted read with the outcome of admitting its ORIGINAL bytes.
#[derive(Debug)]
pub(crate) struct AdmittedRootedRead {
    /// The delivered text (project and gateway transformations applied).
    pub(crate) content: String,
    pub(crate) canonical_path: String,
    /// Classification of the original source, before any redaction.
    pub(crate) classification: lean_ctx_protocol::context_gateway::ClassificationV1,
    /// Whether the original may enter a derived store (complete, not restricted).
    pub(crate) storable: bool,
    /// `false` when the bytes needed lossy decoding.
    pub(crate) complete: bool,
}

#[derive(Debug)]
struct DecodedRead {
    content: String,
    decode_lossy: bool,
}

/// Reads a file as UTF-8 with lossy fallback, enforcing binary detection and max read size limit.
/// Defense-in-depth: verifies that the canonical path stays within the process's project root
/// (if determinable) even though callers SHOULD have already jail-checked the path.
pub fn read_file_lossy(path: &str) -> Result<String, std::io::Error> {
    read_file_internal(path, "ctx_read", false)
}

/// Read and admit the original source for the CLI adapter. Unlike the generic
/// unscoped reader, this explicit agent-facing path always applies the Gateway
/// and carries the decoder's completeness into the admission decision.
pub(crate) fn read_file_lossy_admitted_for_cli(
    path: &str,
) -> Result<crate::core::context_admission::SourceAdmission, std::io::Error> {
    let decoded = read_file_decoded(path)?;
    // The shared Gateway admission composes active project-policy transforms,
    // but it does not emit the existing content-policy audit. Keep that
    // privacy-safe audit on the original bytes; do not feed its transformed
    // text to Gateway classification.
    if let Some(policy) = crate::core::policy::runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(&decoded.content, &policy);
        crate::server::policy_guard::audit_filter("ctx_read", &outcome.audit, outcome.blocked);
    }
    crate::core::context_admission::admit_source_with_storage(
        &decoded.content,
        path,
        false,
        crate::core::context_admission::OutputCompleteness {
            decode_lossy: decoded.decode_lossy,
            ..Default::default()
        },
    )
}

/// Acquire an original source under its project authority through a rooted descriptor.
/// Parsing and derived metadata must use the returned, admitted content.
pub(crate) fn read_file_for_tool_rooted(
    path: &str,
    root: &str,
    tool: &str,
) -> Result<String, std::io::Error> {
    let mut remaining = crate::core::limits::max_read_bytes();
    read_file_for_tool_rooted_budgeted(path, root, tool, &mut remaining)
}

/// Charge original bytes, including rejected sources, before filtering can shrink them.
pub(crate) fn read_file_for_tool_rooted_budgeted(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
) -> Result<String, std::io::Error> {
    read_file_for_tool_rooted_with_path(path, root, tool, remaining_bytes).map(|read| read.content)
}

pub(crate) fn read_file_for_tool_rooted_with_path(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
) -> Result<RootedRead, std::io::Error> {
    read_file_for_tool_rooted_admitted(path, root, tool, remaining_bytes).map(|read| RootedRead {
        content: read.content,
        canonical_path: read.canonical_path,
    })
}

/// Put the source a tool was asked for into the running call's lineage when
/// the answer may not have read it (served from a cache or as a stub).
///
/// Under protection and inside a source capture, the file is read once
/// through the admitted rooted reader — which publishes it, bound to what is
/// on disk now. A source that can no longer be read that way (gone, outside
/// the root, withheld, binary) makes the call's source set unknown. Without a
/// capture or without protection this does nothing and reads nothing.
///
/// The read is not part of the call's delivery: it is kept out of the
/// Decision Receipt so a cached answer is not counted as two admissions.
pub(crate) fn publish_requested_source(path: &str, root: &str, tool: &str) {
    if crate::core::archive::authority::current_capture().is_none()
        || !crate::core::context_admission::stored::source_authority_required()
    {
        return;
    }
    let published = crate::core::context_admission::capture::ADMISSIONS.sync_scope(None, || {
        let mut remaining = crate::core::limits::max_read_bytes();
        read_file_for_tool_rooted_admitted(path, root, tool, &mut remaining).is_ok()
    });
    if !published {
        crate::core::archive::authority::mark_unbound();
    }
}

/// The common rooted read: descriptor-based path jail, budget charged on the
/// original bytes, and one admission decided on the ORIGINAL decoded source.
/// Returns the delivered text together with the outcome of that original
/// admission, so a derived or stored item inherits the source's
/// classification, storability and completeness instead of re-inferring them
/// from text a project rule or the gateway has already rewritten.
pub(crate) fn read_file_for_tool_rooted_admitted(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
) -> Result<AdmittedRootedRead, std::io::Error> {
    read_file_for_tool_rooted_admitted_with(path, root, tool, remaining_bytes, None)
}

/// [`read_file_for_tool_rooted_admitted`] — the same read in every respect —
/// with the gateway rules of a snapshot the caller captured (`Some`) instead
/// of the configuration as it is at this moment (`None`): a caller that
/// judges several things under one snapshot has its sources judged under
/// that snapshot too.
pub(crate) fn read_file_for_tool_rooted_admitted_with(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
    gateway: Option<&crate::core::context_admission::stores::StoreAdmission>,
) -> Result<AdmittedRootedRead, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return Err(std::io::Error::other(
            "source format is not inspectable as text",
        ));
    }
    let authority = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| std::path::PathBuf::from(root));
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(authority)),
        || {
            crate::core::io_boundary::jail_and_check_path(
                tool,
                std::path::Path::new(path),
                std::path::Path::new(root),
            )
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source path withheld by policy",
                )
            })?;
            let (file, canonical_path) =
                open_rooted_nofollow(std::path::Path::new(path), std::path::Path::new(root))?;
            let decoded = read_open_file_lossy_bounded_decoded(
                file,
                path,
                *remaining_bytes,
                Some(remaining_bytes),
            )?;
            let complete = !decoded.decode_lossy;
            let admission = admit_read_with(decoded, path, false, tool, gateway)?;
            let canonical_path = canonical_path.to_string_lossy().into_owned();
            // Source publication (G04): the call that is running learns that
            // this source contributed. A no-op without a capture or without
            // protection; see `archive::authority::publish_read`.
            crate::core::archive::authority::publish_read(
                std::path::Path::new(root),
                &canonical_path,
                tool,
                &admission.text,
            );
            Ok(AdmittedRootedRead {
                content: admission.text,
                canonical_path,
                classification: admission.classification,
                storable: admission.storable && complete,
                complete,
            })
        },
    )
}

/// A rooted source opened ONCE for a read in admitted units: the bounded
/// import read of a history transcript above the read cap (`core::import`).
///
/// The path decision and the open are those of the rooted admitted reader:
/// path jail and secret-path rule first, then one descriptor-relative
/// no-follow open under the root. Everything after that — the size, every
/// byte, every admission — is taken from THIS descriptor; the path is never
/// opened a second time. The raw bytes stay with the caller only until
/// [`Self::admit_unit`] has decided on them.
///
/// ONE rule set judges every unit and seam of the source and is the one the
/// unit-safety inspection reads: the pack through the source view the caller
/// must hold open (`policy::runtime::with_project_source_view` — inside it
/// `policy::runtime::active()` is the pinned snapshot, and the view fails if
/// pack or role changed by its end), the gateway rules through `gateway`,
/// captured once here and passed into every admission.
#[derive(Debug)]
pub(crate) struct RootedUnitSource {
    file: std::fs::File,
    path: String,
    tool: String,
    authority: std::path::PathBuf,
    byte_len: u64,
    gateway: crate::core::context_admission::stores::StoreAdmission,
}

// The gateway rules of this moment as one snapshot — the function the
// database importers use too.
use crate::core::context_admission::carrier::gateway_snapshot as gateway_rules_now;

/// One unit of a [`RootedUnitSource`] after the admission of its ORIGINAL bytes.
#[derive(Debug)]
pub(crate) struct AdmittedUnit {
    /// The delivered text (project and gateway transformations applied).
    pub(crate) content: String,
    /// Whether the original may enter a derived store.
    pub(crate) storable: bool,
}

/// Open `path` under `root` for `tool` for a read in admitted units. Same
/// refusals as [`read_file_for_tool_rooted_admitted`] before its first byte:
/// `PermissionDenied` for a path rule, any other error for a source that is
/// not a readable regular file inside the root — or when the caller holds no
/// source view: without a pinned pack nothing is admitted in units. `gateway`
/// is the caller's ONE snapshot of the gateway rules for this source.
pub(crate) fn open_rooted_unit_source(
    path: &str,
    root: &str,
    tool: &str,
    gateway: crate::core::context_admission::stores::StoreAdmission,
) -> Result<RootedUnitSource, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return Err(std::io::Error::other(
            "source format is not inspectable as text",
        ));
    }
    if crate::core::policy::runtime::inherited_source_view().is_none() {
        return Err(std::io::Error::other(
            "unit admission requires a pinned policy view",
        ));
    }
    let authority = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| std::path::PathBuf::from(root));
    let scope = std::cell::RefCell::new(Some(authority.clone()));
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(scope, || {
        crate::core::io_boundary::jail_and_check_path(
            tool,
            std::path::Path::new(path),
            std::path::Path::new(root),
        )
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source path withheld by policy",
            )
        })?;
        let (file, _) =
            open_rooted_nofollow(std::path::Path::new(path), std::path::Path::new(root))?;
        let byte_len = file.metadata()?.len();
        Ok(RootedUnitSource {
            file,
            path: path.to_owned(),
            tool: tool.to_owned(),
            authority,
            byte_len,
            gateway,
        })
    })
}

impl RootedUnitSource {
    /// Size of the opened file, read from its descriptor.
    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// Whether the gateway rules configured NOW are still the ones every
    /// unit of this source was admitted under (the digest of the admission
    /// fingerprint, `StoreAdmission::digest`). Asked immediately before what
    /// was read is taken over; the pack side is verified by the caller's
    /// source view when it ends.
    pub(crate) fn gateway_rules_unchanged(&self) -> bool {
        gateway_rules_now().digest() == self.gateway.digest()
    }

    /// The next raw bytes of the descriptor; `0` at its end. RAW: nothing read
    /// here may be parsed, stored or delivered before [`Self::admit_unit`] has
    /// admitted the unit it belongs to.
    pub(crate) fn read_raw(&mut self, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        use std::io::Read;
        loop {
            match self.file.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                read => return read,
            }
        }
    }

    /// Admit one unit of this source: the admission the rooted admitted reader
    /// gives a whole file ([`admit_read`]: project policy and gateway on the
    /// ORIGINAL text), for these bytes. The caller cuts units at line ends, so
    /// a unit is complete lines and never divides a character.
    ///
    /// A unit is at most the read cap and must be complete UTF-8: a source
    /// in another encoding cannot be decoded piece by piece and is refused,
    /// not guessed. `PermissionDenied` when the unit is withheld.
    ///
    /// Lineage (G04): a source read in units has no single admitted text to
    /// bind, so under protection the running call's source set becomes
    /// unknown instead of silently smaller.
    ///
    /// Not weaker than the whole-file admission: `ErrorKind::Unsupported`
    /// when a rule that is active now could match across lines without a
    /// bound ([`active_rules_are_unit_safe`]), or when this unit ends inside
    /// a built-in rule whose match would continue in the next unit
    /// ([`unit_ends_inside_a_rule`]). The source is then not admitted in
    /// units at all.
    pub(crate) fn admit_unit(&self, bytes: Vec<u8>) -> Result<AdmittedUnit, std::io::Error> {
        self.admit_bytes(bytes, true)
    }

    /// Admit a SEAM: the end of one unit followed by the start of the next,
    /// as one text, so that a bounded match across the unit boundary is seen
    /// by an admission. Checked only — the caller discards the result's text
    /// and never imports it; a refused seam refuses the source like a
    /// refused unit. A seam ends where the caller cut it, so the
    /// "ends inside a rule" check of a unit does not apply to it.
    pub(crate) fn admit_seam(&self, bytes: Vec<u8>) -> Result<AdmittedUnit, std::io::Error> {
        self.admit_bytes(bytes, false)
    }

    fn admit_bytes(
        &self,
        bytes: Vec<u8>,
        whole_unit: bool,
    ) -> Result<AdmittedUnit, std::io::Error> {
        use crate::core::text_decode::Encoding;
        if bytes.len() > crate::core::limits::max_read_bytes() {
            return Err(std::io::Error::other(
                "source unit exceeded the bounded read limit",
            ));
        }
        if crate::core::text_decode::looks_binary(&bytes) {
            return Err(std::io::Error::other(
                "source format is not inspectable as text",
            ));
        }
        let decoded = crate::core::text_decode::decode_checked(bytes);
        if !decoded.complete() || !matches!(decoded.encoding, Encoding::Utf8 | Encoding::Utf8Bom) {
            return Err(std::io::Error::other(
                "source is not UTF-8 text and cannot be read in units",
            ));
        }
        if whole_unit && unit_ends_inside_a_rule(&decoded.text) {
            return Err(unit_admission_unsupported());
        }
        let scope = std::cell::RefCell::new(Some(self.authority.clone()));
        let admission = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(scope, || {
            // The inspection and the admission read the SAME two objects:
            // the pack pinned by the caller's source view and `self.gateway`.
            if !active_rules_are_unit_safe(&self.gateway) {
                return Err(unit_admission_unsupported());
            }
            admit_read_with(
                DecodedRead {
                    content: decoded.text,
                    decode_lossy: false,
                },
                &self.path,
                false,
                &self.tool,
                Some(&self.gateway),
            )
        })?;
        if crate::core::archive::authority::current_capture().is_some()
            && crate::core::context_admission::stored::source_authority_required()
        {
            crate::core::archive::authority::mark_unbound();
        }
        Ok(AdmittedUnit {
            content: admission.text,
            storable: admission.storable,
        })
    }
}

fn unit_admission_unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "source cannot be admitted in units under the active rules",
    )
}

/// Whether every rule that is active NOW and that lean-ctx does not ship
/// itself is proven unable to match across a line end — the condition under
/// which admitting complete lines unit by unit finds what admitting the whole
/// text finds.
///
/// - Policy pack `[redaction]` entries and `[filters.blocked_patterns]`, and
///   the configured `secret_detection.custom_patterns`: free regular
///   expressions. The pack parser bounds their number and their LENGTH
///   (`MAX_RULE_PATTERN_BYTES`), not what they match: any flag is allowed and
///   no match length is bounded. Each must pass [`pattern_is_single_line`].
/// - A semantic detector stage judges a document, not lines: when one is
///   configured, nothing is admitted in units.
///
/// The built-in rules are not covered here: they are line-local, or bounded
/// and covered by the seam, or caught by [`unit_ends_inside_a_rule`].
fn active_rules_are_unit_safe(
    gateway: &crate::core::context_admission::stores::StoreAdmission,
) -> bool {
    let pack = crate::core::policy::runtime::active().is_none_or(|active| {
        active
            .redaction
            .iter()
            .chain(&active.blocked_patterns)
            .all(|(_, pattern)| pattern_is_single_line(pattern.as_str()))
    });
    let rules = gateway.admission_policy();
    pack && rules.semantic.is_none()
        && rules
            .secret_patterns
            .custom_patterns
            .iter()
            .all(|pattern| pattern_is_single_line(pattern))
}

// The one statement of what a rule can match across a line end, shared with
// the gateway's chunked scan.
use crate::core::context_admission::cross_line::{
    ends_inside_classification_field, pattern_is_single_line,
};

/// Whether a unit ends INSIDE a built-in rule whose match the whole-file
/// admission would continue in the following text — the built-in rules whose
/// reach across lines no window bounds:
///
/// - the private-key block mask (`core::redaction`, "Private key block"):
///   from a PEM `BEGIN … PRIVATE KEY` header line to the first END marker
///   anywhere after it, or over the key-material lines that follow. Open when
///   the unit ends inside a header, or holds a header with no END marker
///   after it. Checked on the text as that rule sees it
///   (`normalize_secret_text`).
/// - the classification field (`input_filters::classification::field_re`):
///   `classification` / `sensitivity` / `data class`, `:` or `=`, and the
///   value, separated by any amount of white space including line ends. Open
///   when the unit ends after the key, or after the separator.
///
/// Decided on the unit alone; never on a seam.
fn unit_ends_inside_a_rule(text: &str) -> bool {
    use std::sync::OnceLock;
    const KINDS: &str = r"(?:RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----";
    static OPEN_HEADER: OnceLock<regex::Regex> = OnceLock::new();
    static HEADER: OnceLock<regex::Regex> = OnceLock::new();
    static END: OnceLock<regex::Regex> = OnceLock::new();
    let built = |cell: &'static OnceLock<regex::Regex>, pattern: &str| {
        cell.get_or_init(|| regex::Regex::new(pattern).expect("valid built-in rule guard"))
    };

    // The same statement of the open field the gateway's chunked scan uses.
    if ends_inside_classification_field(text) {
        return true;
    }
    let normalized = crate::core::redaction::normalize_secret_text(text);
    if !normalized.contains("-----BEGIN") {
        return false;
    }
    let open_header = built(
        &OPEN_HEADER,
        r"-----BEGIN(?:\s+(?:RSA|EC|DSA|OPENSSH))?(?:\s+PRIVATE)?\s*\z",
    );
    if open_header.is_match(&normalized) {
        return true;
    }
    let last_header = built(&HEADER, &format!(r"-----BEGIN\s+{KINDS}"))
        .find_iter(&normalized)
        .last()
        .map(|found| found.end());
    let last_end = built(&END, &format!(r"-----END\s+{KINDS}"))
        .find_iter(&normalized)
        .last()
        .map(|found| found.start());
    // Every header needs an END marker after it, with key material between.
    match (last_header, last_end) {
        (Some(header), Some(end)) => header >= end,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The rooted no-follow open on its own, for a source that is not read as
/// text (a history database, `core::import::database`): one open, relative to
/// the root's descriptor, no link followed in any component on Unix, a regular
/// file. The caller has made the path decision and runs every further check
/// on the returned descriptor.
pub(crate) fn open_rooted_source(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<std::fs::File, std::io::Error> {
    open_rooted_nofollow(path, root).map(|(file, _)| file)
}

fn read_file_internal(
    path: &str,
    tool: &str,
    enforce_policy: bool,
) -> Result<String, std::io::Error> {
    let decoded = read_file_decoded(path)?;
    apply_read_policies(decoded, path, false, tool, enforce_policy)
}

fn read_file_decoded(path: &str) -> Result<DecodedRead, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        let msg = crate::core::binary_detect::binary_file_message(path);
        return Err(std::io::Error::other(msg));
    }

    {
        let canonical =
            crate::core::pathutil::safe_canonicalize_bounded(std::path::Path::new(path), 2000);
        if let Some(cwd) = crate::core::policy::diagnostics::project() {
            let root = crate::core::pathutil::safe_canonicalize_bounded(&cwd, 2000);
            if !canonical.starts_with(&root) {
                let allow = crate::core::pathjail::allow_paths_from_env_and_config();
                let data_dir_ok = crate::core::data_dir::lean_ctx_data_dir()
                    .is_ok_and(|d| canonical.starts_with(d));
                let tmp_ok = canonical.starts_with(std::env::temp_dir());
                if !allow.iter().any(|a| canonical.starts_with(a)) && !data_dir_ok && !tmp_ok {
                    // Internal file-read workers may lack the MCP request scope.
                    // Never put an untrusted path into this fallback diagnostic.
                    tracing::warn!("defense-in-depth: read target may escape current project root");
                }
            }
        }
    }

    let file = open_with_retry(path)?;
    read_open_file_lossy_bounded_decoded(file, path, crate::core::limits::max_read_bytes(), None)
}

pub(crate) fn read_file_lossy_rooted(path: &str, root: &str) -> Result<RootedRead, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return Err(std::io::Error::other(
            crate::core::binary_detect::binary_file_message(path),
        ));
    }
    let (file, canonical_path) =
        open_rooted_nofollow(std::path::Path::new(path), std::path::Path::new(root))?;
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(std::path::PathBuf::from(root))),
        || {
            Ok(RootedRead {
                // A signed source snapshot must retain its original digest. If
                // protection requires rewriting it, withhold this legacy path.
                content: read_open_file_lossy(file, path, true, "ctx_read", true)?,
                canonical_path: canonical_path.to_string_lossy().into_owned(),
            })
        },
    )
}

fn read_open_file_lossy(
    file: std::fs::File,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    enforce_policy: bool,
) -> Result<String, std::io::Error> {
    read_open_file_lossy_bounded(
        file,
        path,
        preserve_bytes,
        tool,
        enforce_policy,
        crate::core::limits::max_read_bytes(),
        None,
    )
}

fn read_open_file_lossy_bounded(
    file: std::fs::File,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    enforce_policy: bool,
    maximum_bytes: usize,
    remaining_bytes: Option<&mut usize>,
) -> Result<String, std::io::Error> {
    let decoded = read_open_file_lossy_bounded_decoded(file, path, maximum_bytes, remaining_bytes)?;
    apply_read_policies(decoded, path, preserve_bytes, tool, enforce_policy)
}

fn read_open_file_lossy_bounded_decoded(
    file: std::fs::File,
    path: &str,
    maximum_bytes: usize,
    remaining_bytes: Option<&mut usize>,
) -> Result<DecodedRead, std::io::Error> {
    let bytes = read_open_file_bounded_bytes(file, path, maximum_bytes, remaining_bytes)?;
    // Same decoding as the index readers, so every file `ctx_compose` or
    // `ctx_search` can return is readable here too (Windows ANSI, UTF-16).
    // The decoder says what it could not represent: corrupt UTF-8, a dangling
    // UTF-16 byte, an unpaired surrogate. Windows-1252 and well-formed UTF-16
    // decode faithfully, so policy scanning sees every byte of them.
    let decoded = crate::core::text_decode::decode_checked(bytes);
    let decode_lossy = !decoded.complete();
    Ok(DecodedRead {
        content: decoded.text,
        decode_lossy,
    })
}

/// The bytes of an opened text source, bounded by `maximum_bytes` and the
/// read cap and charged to the call's budget. RAW: nothing of them is parsed,
/// stored or delivered before an admission has decided on the source.
fn read_open_file_bounded_bytes(
    file: std::fs::File,
    path: &str,
    maximum_bytes: usize,
    remaining_bytes: Option<&mut usize>,
) -> Result<Vec<u8>, std::io::Error> {
    let cap = maximum_bytes.min(crate::core::limits::max_read_bytes());
    if cap == 0 {
        return Err(std::io::Error::other("source read budget exhausted"));
    }
    let meta = file
        .metadata()
        .map_err(|e| std::io::Error::other(format!("cannot stat open file descriptor: {e}")))?;
    if meta.len() > cap as u64 {
        return Err(std::io::Error::other(format!(
            "file too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
             Increase the limit or use a line-range read: mode=\"lines:1-100\"",
            meta.len(),
            cap
        )));
    }

    use std::io::{BufRead, Read};
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let mut reader = std::io::BufReader::with_capacity(cap.saturating_add(1).min(8192), file);
    let prefix = reader.fill_buf()?;
    if crate::core::text_decode::looks_binary(prefix) {
        if let Some(remaining) = remaining_bytes {
            *remaining = remaining.saturating_sub(prefix.len());
        }
        return Err(std::io::Error::other(
            crate::core::binary_detect::binary_file_message(path),
        ));
    }
    let read_result = reader.take(cap as u64 + 1).read_to_end(&mut bytes);
    if let Some(remaining) = remaining_bytes {
        *remaining = remaining.saturating_sub(bytes.len());
    }
    read_result?;
    if bytes.len() > cap {
        return Err(std::io::Error::other(
            "file exceeded the bounded read limit",
        ));
    }
    Ok(bytes)
}

fn apply_read_policies(
    decoded: DecodedRead,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    enforce_policy: bool,
) -> Result<String, std::io::Error> {
    let s = decoded.content;
    // Constrain agent acquisitions, not unrelated human/local filesystem reads.
    // Detached read workers inherit this scope from TaskSpine::spawn_thread.
    let agent_acquisition = enforce_policy
        || crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().is_some())
            .unwrap_or(false);
    if !agent_acquisition {
        return Ok(s);
    }
    let admission = admit_read(
        DecodedRead {
            content: s,
            decode_lossy: decoded.decode_lossy,
        },
        path,
        preserve_bytes,
        tool,
    )?;
    // Source publication for the readers that are not handed a root (the main
    // `ctx_read` path): the request project is the authority the read ran
    // under. Without one the source cannot be bound, which — under protection
    // and inside a capture — makes the call's source set unknown.
    if crate::core::archive::authority::current_capture().is_some() {
        let project = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .unwrap_or_default();
        let canonical =
            crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(path));
        crate::core::archive::authority::publish_read(
            &project,
            &canonical.to_string_lossy(),
            tool,
            &admission.text,
        );
    }
    Ok(admission.text)
}

/// Admit one decoded source. Order, defined once for every reader:
///
/// 1. the project policy audits and judges the ORIGINAL text (a block, or a
///    lossy decode under a pack, refuses);
/// 2. the gateway admission also decides on the ORIGINAL text, with the
///    decoder's completeness — classification, secrets, storability. A project
///    redaction can therefore never remove a marking before the gateway has
///    classified the source;
/// 3. the delivered text is the composition of both transformations, the most
///    restrictive result winning (`admit_source_with_storage` composes the
///    project rewrite itself and withholds when the two do not commute).
///
/// Clean content is returned byte-identically.
fn admit_read(
    decoded: DecodedRead,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
) -> Result<crate::core::context_admission::SourceAdmission, std::io::Error> {
    admit_read_with(decoded, path, preserve_bytes, tool, None)
}

/// [`admit_read`] with the gateway rules of a snapshot the caller captured
/// once, instead of the configuration as it is at this moment: a source that
/// is admitted in several pieces is judged by ONE rule set (the pack side is
/// pinned by the caller's source view, `policy::runtime::with_source_view`).
fn admit_read_with(
    decoded: DecodedRead,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    gateway: Option<&crate::core::context_admission::stores::StoreAdmission>,
) -> Result<crate::core::context_admission::SourceAdmission, std::io::Error> {
    let original = decoded.content;
    if let Some(policy) = crate::core::policy::runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(&original, &policy);
        crate::server::policy_guard::audit_filter(tool, &outcome.audit, outcome.blocked);
        if decoded.decode_lossy || outcome.blocked || (preserve_bytes && outcome.text != original) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source content withheld by policy",
            ));
        }
    }
    // Context Gateway admission runs before any cache, compression or
    // delivery sees the source, for every read mode.
    let completeness = crate::core::context_admission::OutputCompleteness {
        decode_lossy: decoded.decode_lossy,
        ..Default::default()
    };
    match gateway {
        Some(snapshot) => crate::core::context_admission::admit_source_with_snapshot(
            &original,
            path,
            preserve_bytes,
            completeness,
            snapshot,
        ),
        None => crate::core::context_admission::admit_source_with_storage(
            &original,
            path,
            preserve_bytes,
            completeness,
        ),
    }
}

#[cfg(unix)]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let root = crate::core::pathutil::canonicalize_secure(root)?;
    let target = crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err(std::io::Error::other(
            "rooted read target must be a regular file",
        ));
    }
    let root_name = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("rooted read path contains a NUL byte"))?;
    // SAFETY: root_name is NUL-terminated; File takes ownership immediately.
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: root_fd is a new successful descriptor owned by this scope.
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err(std::io::Error::other(
                "rooted read path contains an invalid component",
            ));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::other("rooted read path contains a NUL byte"))?;
        let last = index + 1 == components.len();
        let flags = if last {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        // SAFETY: directory is live and name is NUL-terminated. File owns success.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd is a new successful descriptor owned by this scope.
        let opened = unsafe { std::fs::File::from_raw_fd(fd) };
        if last {
            if !opened.metadata()?.is_file() {
                return Err(std::io::Error::other(
                    "rooted read target must be a regular file",
                ));
            }
            return Ok((opened, target));
        }
        directory = opened;
    }
    unreachable!("non-empty rooted path must return from its final component")
}

#[cfg(windows)]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let root = crate::core::pathutil::canonicalize_secure(root)?;
    // Open the path the jail checked: a relative path resolves against the
    // root there, not against the process CWD (the daemon's is not the project).
    let target = crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let file = std::fs::OpenOptions::new().read(true).open(&target)?;
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: the handle is live and buffer is writable for its full length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if length == 0 || length as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    let final_path = crate::core::pathutil::strip_verbatim(std::path::PathBuf::from(
        OsString::from_wide(&buffer),
    ));
    if !final_path.starts_with(&root) || !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "opened file escaped the rooted read boundary",
        ));
    }
    Ok((file, final_path))
}

#[cfg(not(any(unix, windows)))]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    let root = crate::core::pathutil::canonicalize_secure(root)?;
    let target = crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let file = std::fs::File::open(&target)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "rooted read target must be a regular file",
        ));
    }
    Ok((file, target))
}

/// A streamed line-window read (#811): only the requested span's raw lines,
/// plus the file's true total line count for the header — the rest of the
/// file is never buffered.
pub(super) struct LineWindow {
    /// Raw lines within `[start, end]`, joined with `\n`.
    pub(super) body: String,
    /// True total line count of the file.
    pub(super) total_lines: usize,
    /// Clamped, 1-based inclusive bounds actually served.
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Parses an `anchored:` window payload for the disk-streaming short-circuit
/// below. Only the dash form (`"N-M"`) is fast-pathed — `anchored_lines_mode`
/// (the registered handler) always emits it, using the `999999` EOF sentinel
/// rather than a bare `"N"`. A hand-typed bare payload (meaning "N to EOF")
/// returns `None` and falls through to the normal full-read path instead of
/// guessing a total line count up front.
pub(super) fn parse_disk_anchor_range(payload: &str) -> Option<(usize, usize)> {
    let (s, e) = payload.split_once('-')?;
    let start = s.trim().parse::<usize>().ok()?.max(1);
    let end = e.trim().parse::<usize>().ok()?;
    Some((start, end))
}

/// Streams `path` line-by-line and extracts only `[start, end]` (1-based,
/// inclusive) without ever holding the whole file in memory — the
/// anchored-window counterpart to [`read_file_lossy`] (#811). Every line is
/// still counted (one cheap UTF-8 pass, no per-line allocation outside the
/// requested window) so the caller can report the true total. Returns `None`
/// on anything that isn't a clean streamed text read (I/O error, a binary
/// file, invalid UTF-8 anywhere in the file) so the caller can fall back to
/// the existing, more permissive `read_file_lossy` path — behaviour never
/// regresses, it just doesn't always get the fast path.
pub(super) fn read_line_window(path: &str, start: usize, end: usize) -> Option<LineWindow> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return None;
    }
    use std::io::BufRead;
    let file = open_with_retry(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    if reader
        .fill_buf()
        .ok()?
        .iter()
        .take(8192)
        .any(|byte| *byte == 0)
    {
        return None;
    }
    let mut total = 0usize;
    let mut collected = Vec::new();
    for line in reader.lines() {
        let line = line.ok()?;
        total += 1;
        if total >= start && total <= end {
            collected.push(line);
        }
    }
    Some(LineWindow {
        body: collected.join("\n"),
        total_lines: total,
        start: start.min(total.max(1)),
        end: end.min(total),
    })
}

/// #811: attempt the disk-streaming short-circuit for a fresh `anchored:N-M`
/// read. `None` when the request isn't eligible (not a windowed anchored
/// read, or a preread is already in hand — nothing to short-circuit) or the
/// fast path can't run cleanly (binary file, invalid UTF-8, I/O error); the
/// caller falls through to the normal full-read path in that case.
pub(super) fn try_disk_anchored_window(
    path: &str,
    mode: &str,
    fresh: bool,
    preread_is_none: bool,
    file_ref: &str,
    short: &str,
) -> Option<ReadOutput> {
    if !fresh || !preread_is_none {
        return None;
    }
    let range = mode.strip_prefix("anchored:")?;
    let (start, end) = parse_disk_anchor_range(range)?;
    let window = read_line_window(path, start, end)?;
    let (out, _) = format_anchored_output_window(
        file_ref,
        short,
        &window.body,
        window.total_lines,
        Some((window.start, window.end)),
    );
    let out = crate::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    Some(ReadOutput {
        content: out,
        resolved_mode: mode.to_string(),
        output_tokens: sent,
        is_cache_hit: false,
    })
}

/// Opens a file, retrying once after a brief pause on NotFound.
/// Works around overlay/FUSE stat-cache races in container runtimes (Docker, Codex).
/// Uses O_NOFOLLOW on Unix for TOCTOU symlink protection.
fn open_with_retry(path: &str) -> Result<std::fs::File, std::io::Error> {
    match open_nofollow(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            open_nofollow(path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    let hint = crate::core::path_suggest::hint(std::path::Path::new(path));
                    if hint.is_empty() {
                        std::io::Error::other(format!(
                            "file not found: {path} — verify the path with ctx_tree or ctx_search"
                        ))
                    } else {
                        std::io::Error::other(format!("file not found: {path}{hint}"))
                    }
                } else {
                    e
                }
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    let p = Path::new(path);
    // Canonicalize the parent directory (resolving symlinks in the directory path)
    // but apply O_NOFOLLOW only to the final file component. This prevents
    // symlink-following attacks on the target file while allowing legitimate
    // directory symlinks (e.g., /tmp → /private/tmp on macOS).
    if let (Some(parent), Some(filename)) = (p.parent(), p.file_name())
        && parent.exists()
    {
        let canonical_parent = crate::core::pathutil::safe_canonicalize_bounded(parent, 2000);
        let canonical_path = canonical_parent.join(filename);
        return std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical_path);
    }

    // Fallback: direct open with O_NOFOLLOW
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    std::fs::File::open(path)
}

#[cfg(test)]
#[path = "file_io_admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
mod tests {
    use super::read_file_lossy_rooted;

    #[test]
    fn original_read_budget_counts_masked_and_blocked_content() {
        let root = tempfile::tempdir().unwrap();
        let policy_path = root.path().join("policy.toml");
        std::fs::write(&policy_path,
            "name = 'budget'\nversion = '1.0.0'\ndescription = 'fixture'\n[redaction]\nmasked = 'X+'\n[filters]\nclassification = 'block'\n").unwrap();
        let pack = crate::core::policy::parse_file(&policy_path).unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
            crate::core::policy::resolve(&pack).unwrap(),
        ));
        let masked = root.path().join("masked.txt");
        let blocked = root.path().join("blocked.txt");
        std::fs::write(&masked, "X".repeat(512)).unwrap();
        let blocked_text = format!("CONFIDENTIAL{}", " ".repeat(500));
        std::fs::write(&blocked, &blocked_text).unwrap();
        let mut remaining = 512 + blocked_text.len();
        let content = super::read_file_for_tool_rooted_budgeted(
            masked.to_str().unwrap(),
            root.path().to_str().unwrap(),
            "ctx_symbol",
            &mut remaining,
        )
        .unwrap();
        assert!(content.len() < 512);
        assert_eq!(remaining, blocked_text.len());
        assert!(
            super::read_file_for_tool_rooted_budgeted(
                blocked.to_str().unwrap(),
                root.path().to_str().unwrap(),
                "ctx_symbol",
                &mut remaining,
            )
            .is_err()
        );
        assert_eq!(remaining, 0);
        assert!(
            super::read_file_for_tool_rooted_budgeted(
                masked.to_str().unwrap(),
                root.path().to_str().unwrap(),
                "ctx_symbol",
                &mut remaining,
            )
            .is_err()
        );
    }

    #[test]
    fn rooted_read_binds_content_to_canonical_in_root_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.txt");
        std::fs::write(&path, "rooted content").unwrap();

        let read = read_file_lossy_rooted(&path.to_string_lossy(), &root.path().to_string_lossy())
            .unwrap();

        assert_eq!(read.content, "rooted content");
        assert_eq!(
            std::path::Path::new(&read.canonical_path),
            crate::core::pathutil::canonicalize_secure(&path).unwrap()
        );
    }

    #[test]
    fn rooted_read_resolves_a_relative_path_against_the_root_not_the_cwd() {
        // Config (path_jail) is read from the data dir parallel tests repoint.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("relative.txt"), "from the root").unwrap();

        let read = read_file_lossy_rooted("relative.txt", &root.path().to_string_lossy()).unwrap();

        assert_eq!(read.content, "from the root");
    }

    #[test]
    fn rooted_read_rejects_outside_source_and_binary_content() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.txt");
        std::fs::write(&outside_path, "outside").unwrap();
        assert!(
            read_file_lossy_rooted(
                &outside_path.to_string_lossy(),
                &root.path().to_string_lossy(),
            )
            .is_err()
        );

        let binary = root.path().join("binary.txt");
        std::fs::write(&binary, b"text\0binary").unwrap();
        let error =
            read_file_lossy_rooted(&binary.to_string_lossy(), &root.path().to_string_lossy())
                .unwrap_err();
        assert!(error.to_string().contains("Binary file detected"));
    }

    #[cfg(unix)]
    #[test]
    fn rooted_read_resolves_safe_alias_and_rejects_escape_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("inside.txt");
        std::fs::write(&inside, "inside").unwrap();
        let safe_alias = root.path().join("safe-alias.txt");
        symlink(&inside, &safe_alias).unwrap();
        assert_eq!(
            read_file_lossy_rooted(
                &safe_alias.to_string_lossy(),
                &root.path().to_string_lossy(),
            )
            .unwrap()
            .content,
            "inside"
        );

        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.txt");
        std::fs::write(&outside_path, "outside").unwrap();
        let escape = root.path().join("escape.txt");
        symlink(outside_path, &escape).unwrap();
        assert!(
            read_file_lossy_rooted(&escape.to_string_lossy(), &root.path().to_string_lossy(),)
                .is_err()
        );
    }
}
