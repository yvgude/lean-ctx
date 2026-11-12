// SPDX-License-Identifier: Apache-2.0
//! Unit claims: which agent is editing which function or section of a file.
//!
//! The edit tools take a short claim on the innermost units (function,
//! method, type, markdown section) an edit touches, and check the claims of
//! other agents before they write. Another agent's claim on a touched unit
//! is a conflict: by default the edit goes ahead with a warning naming the
//! holder (`warn`); `LEAN_CTX_CLAIM_MODE=strict` refuses it; `off` disables
//! both the check and the claims.
//!
//! Claims are ordinary machine-wide leases ([`crate::core::agent_lease`]) of
//! kind `Symbol` with a readable reference
//! `project-unit:<repo>/<path>#<unit>`, so stores and processes that predate
//! unit claims read them unchanged. `<repo>` names the repository, not the
//! checkout: linked worktrees of one repository share it, so two agents in
//! two worktrees meet on the same unit. A claim lives ten minutes from the
//! last edit that touched the unit and is released when its agent finishes.
//!
//! This module detects, reports and hints only. Planning a task against the
//! claims is an optional external planner's job (`weave plan-claims`), fed
//! the [`snapshot`] in the `leanctx.claims.v1` contract.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::agent_lease::{
    AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1, AgentLeaseResourceKindV1,
    AgentLeaseV1,
};

/// Lease reference scheme of a unit claim.
pub const UNIT_SCHEME: &str = "project-unit:";
/// Unit name of a whole-file claim.
pub const WHOLE_FILE: &str = "*";
/// How long a claim lives after the last edit that touched its unit.
pub const CLAIM_TTL_MS: u64 = 10 * 60 * 1_000;
/// Schema of the snapshot handed to an external planner.
pub const CLAIMS_SCHEMA: &str = "leanctx.claims.v1";
const MAX_REF_BYTES: usize = 256;

/// What the edit tools do with another agent's claim on a touched unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimMode {
    Off,
    Warn,
    Strict,
}

/// `LEAN_CTX_CLAIM_MODE` (`off`, `warn`, `strict`); `warn` when unset or unknown.
#[must_use]
pub fn mode() -> ClaimMode {
    match std::env::var("LEAN_CTX_CLAIM_MODE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" | "0" | "false" => ClaimMode::Off,
        "strict" | "enforce" | "refuse" => ClaimMode::Strict,
        _ => ClaimMode::Warn,
    }
}

// ─── Resources ───────────────────────────────────────────────────────────────

/// Repository scope of `project_root`: its project identity (a linked
/// worktree follows its main checkout's origin), else the checkout itself.
#[must_use]
pub fn repo_scope(project_root: &str) -> String {
    let identity = crate::core::project_hash::project_identity(project_root)
        .unwrap_or_else(|| crate::core::project_hash::hash_project_root(project_root));
    blake3::hash(identity.as_bytes()).to_hex()[..16].to_string()
}

fn encode(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b':') {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Lease reference of unit `unit` of the repository-relative file `rel`;
/// `None` when it would exceed the lease reference bound.
#[must_use]
pub fn unit_resource(scope: &str, rel: &str, unit: &str) -> Option<String> {
    let mut out = format!("{UNIT_SCHEME}{scope}/");
    encode(&mut out, rel);
    out.push('#');
    encode(&mut out, unit);
    (out.len() <= MAX_REF_BYTES).then_some(out)
}

/// `(scope, rel, unit)` of a unit claim reference.
#[must_use]
pub fn parse_unit_resource(resource: &str) -> Option<(String, String, String)> {
    let rest = resource.strip_prefix(UNIT_SCHEME)?;
    let (scope, rest) = rest.split_once('/')?;
    let (rel, unit) = rest.rsplit_once('#')?;
    Some((scope.to_string(), decode(rel)?, decode(unit)?))
}

/// Repository-relative spelling of `path` under `project_root` (`/` separated).
#[must_use]
pub fn relative_path(project_root: &str, path: &Path) -> Option<String> {
    let root = Path::new(project_root);
    let rel = path
        .strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let root = crate::core::pathutil::safe_canonicalize_or_self(root);
            let path = path
                .parent()
                .map(crate::core::pathutil::safe_canonicalize_or_self)
                .map(|p| p.join(path.file_name().unwrap_or_default()))?;
            path.strip_prefix(root).ok().map(Path::to_path_buf)
        })?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    (!rel.is_empty() && !rel.split('/').any(|p| p == "..")).then_some(rel)
}

// ─── Units of a file ─────────────────────────────────────────────────────────

/// One named unit of a file: 1-based inclusive line span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitSpan {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

fn markdown_sections(content: &str) -> Vec<UnitSpan> {
    let lines: Vec<&str> = content.lines().collect();
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    let mut fence = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && line[level..].starts_with(' ') {
            heads.push((i + 1, level, line[level..].trim().to_string()));
        }
    }
    heads
        .iter()
        .enumerate()
        .map(|(k, (start, level, name))| {
            let end = heads[k + 1..]
                .iter()
                .find(|(_, l, _)| l <= level)
                .map_or(lines.len(), |(s, _, _)| s - 1);
            UnitSpan {
                name: name.clone(),
                start: *start,
                end: end.max(*start),
            }
        })
        .collect()
}

/// Named units of `content` (the file `path`): markdown sections, else the
/// spans of the signatures tree-sitter (or the regex fallback) finds.
#[must_use]
pub fn units_of(path: &str, content: &str) -> Vec<UnitSpan> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "md" | "markdown" | "mdx") {
        return markdown_sections(content);
    }
    // Uncounted: a claim check must not write grammar telemetry per edit.
    crate::core::signatures::extract_signatures_counted(content, &ext)
        .0
        .into_iter()
        .filter_map(|sig| {
            let start = sig.start_line?;
            let end = sig.end_line.unwrap_or(start).max(start);
            (!sig.name.is_empty()).then_some(UnitSpan {
                name: sig.name,
                start,
                end,
            })
        })
        .collect()
}

/// Line spans an edit touches, on each side: `(old, new)`, 1-based inclusive.
/// A pure insertion before line `L` is the empty span `(L, L - 1)`.
#[must_use]
pub fn changed_spans(before: &str, after: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut old = Vec::new();
    let mut new = Vec::new();
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            continue;
        }
        old.push((o.start + 1, o.end));
        new.push((n.start + 1, n.end));
    }
    (old, new)
}

fn touches(unit: &UnitSpan, (start, end): (usize, usize)) -> bool {
    if end < start {
        // Insertion between lines `start - 1` and `start`: inside only when
        // both neighbours are.
        unit.start < start && start <= unit.end
    } else {
        unit.start <= end && start <= unit.end
    }
}

/// Innermost units each span touches (a span that touches none adds nothing).
#[must_use]
pub fn touched_units(units: &[UnitSpan], spans: &[(usize, usize)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for &span in spans {
        let hit: Vec<&UnitSpan> = units.iter().filter(|u| touches(u, span)).collect();
        for u in &hit {
            let inner = hit.iter().any(|v| {
                v != u
                    && v.start >= u.start
                    && v.end <= u.end
                    && (v.end - v.start) < (u.end - u.start)
            });
            if !inner {
                out.insert(u.name.clone());
            }
        }
    }
    out
}

/// What an edit of `path` from `before` to `after` touches: the innermost
/// unit names on either side, and the old-side spans.
#[must_use]
pub fn edit_footprint(
    path: &str,
    before: &str,
    after: &str,
) -> (BTreeSet<String>, Vec<(usize, usize)>) {
    let (old, new) = changed_spans(before, after);
    let mut names = touched_units(&units_of(path, before), &old);
    names.extend(touched_units(&units_of(path, after), &new));
    (names, old)
}

// ─── Conflicts ───────────────────────────────────────────────────────────────

/// Another agent's claim on something an edit touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub holder: String,
    pub path: String,
    pub unit: String,
    pub expires_at_epoch_ms: u64,
}

fn other_live(leases: Vec<AgentLeaseV1>, agent_id: &str) -> impl Iterator<Item = AgentLeaseV1> {
    let agent_id = agent_id.to_string();
    leases
        .into_iter()
        .filter(move |lease| lease.request.owner_agent_id != agent_id)
}

/// Claims of other agents that an edit of `rel` touching `names` (and the
/// old-side `spans` of `before`) collides with: unit claims on the same
/// unit, on a unit containing a touched line, or on the whole file, and
/// path leases on the file or a directory above it.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn conflicts_in(
    leases: Vec<AgentLeaseV1>,
    project_root: &str,
    scope: &str,
    agent_id: &str,
    rel: &str,
    before: &str,
    names: &BTreeSet<String>,
    spans: &[(usize, usize)],
) -> Vec<Conflict> {
    let rel_folded = rel.to_lowercase();
    let mut before_units: Option<Vec<UnitSpan>> = None;
    let mut out = Vec::new();
    for lease in other_live(leases, agent_id) {
        let held = &lease.request.resource_ref;
        let unit = match lease.request.resource_kind {
            AgentLeaseResourceKindV1::Symbol => {
                let Some((s, path, unit)) = parse_unit_resource(held) else {
                    continue;
                };
                if s != scope || path != rel {
                    continue;
                }
                let hit = unit == WHOLE_FILE
                    || names.contains(&unit)
                    || before_units
                        .get_or_insert_with(|| units_of(rel, before))
                        .iter()
                        .any(|u| u.name == unit && spans.iter().any(|s| touches(u, *s)));
                if !hit {
                    continue;
                }
                unit
            }
            AgentLeaseResourceKindV1::Path => {
                let Some(path) = crate::core::agent_lease::project_path_of(project_root, held)
                else {
                    continue;
                };
                let path = decode(&path).unwrap_or(path).to_lowercase();
                if rel_folded != path && !rel_folded.starts_with(&format!("{path}/")) {
                    continue;
                }
                WHOLE_FILE.to_string()
            }
            AgentLeaseResourceKindV1::WorkGraphNode => continue,
        };
        out.push(Conflict {
            holder: lease.request.owner_agent_id.clone(),
            path: rel.to_string(),
            unit,
            expires_at_epoch_ms: lease.expires_at_epoch_ms,
        });
    }
    out.sort_by(|a, b| (&a.holder, &a.unit).cmp(&(&b.holder, &b.unit)));
    out.dedup();
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One line per conflict for the agent, naming holder, unit and what to do.
#[must_use]
pub fn render_conflicts(conflicts: &[Conflict], refused: bool) -> String {
    let now = now_ms();
    let mut out = if refused {
        "ERROR: claim conflict (LEAN_CTX_CLAIM_MODE=strict), nothing was written:\n".to_string()
    } else {
        "WARNING: claim conflict, another agent is editing what this edit touched:\n".to_string()
    };
    for c in conflicts {
        let unit = if c.unit == WHOLE_FILE {
            "the whole file".to_string()
        } else {
            format!("unit `{}`", c.unit)
        };
        let _ = writeln!(
            out,
            "  {}: {unit} is claimed by {} for {}s more",
            c.path,
            c.holder,
            c.expires_at_epoch_ms.saturating_sub(now) / 1_000
        );
    }
    out.push_str(
        "  Alternative: edit a different unit, wait until the holder finishes \
         (ctx_agent action=claims shows holders), or message the holder; \
         ctx_agent action=plan_claims plans a task around held units.",
    );
    out
}

// ─── Claiming and releasing ──────────────────────────────────────────────────

fn request(agent_id: &str, resource_ref: String) -> AgentLeaseRequestV1 {
    AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!(
            "lease-request:{}",
            blake3::hash(format!("{agent_id}:{resource_ref}").as_bytes()).to_hex()
        ),
        resource_kind: AgentLeaseResourceKindV1::Symbol,
        resource_ref,
        owner_agent_id: agent_id.to_string(),
        duration_ms: CLAIM_TTL_MS,
    }
}

/// Claims `units` of `rel` for `agent_id` (or extends its own claims to a
/// full TTL). Units another agent holds are skipped. Returns the units now
/// held.
pub fn claim(scope: &str, agent_id: &str, rel: &str, units: &BTreeSet<String>) -> Vec<String> {
    let mut held = Vec::new();
    for unit in units {
        let Some(resource) = unit_resource(scope, rel, unit) else {
            continue;
        };
        match crate::core::agent_lease::acquire_shared(request(agent_id, resource.clone())) {
            Ok(AgentLeaseAcquireV1::Granted(lease)) => {
                if lease.expires_at_epoch_ms.saturating_sub(now_ms()) < CLAIM_TTL_MS / 2 {
                    let _ = crate::core::agent_lease::renew_shared(
                        AgentLeaseResourceKindV1::Symbol,
                        &resource,
                        agent_id,
                        &lease.lease_ref,
                        CLAIM_TTL_MS,
                    );
                }
                held.push(unit.clone());
            }
            Ok(AgentLeaseAcquireV1::HeldBy { .. }) | Err(_) => {}
        }
    }
    held
}

/// Releases every unit claim `agent_id` holds. Returns how many.
pub fn release_agent(agent_id: &str) -> usize {
    let Ok(leases) = crate::core::agent_lease::active_shared() else {
        return 0;
    };
    leases
        .into_iter()
        .filter(|l| {
            l.request.owner_agent_id == agent_id
                && l.request.resource_kind == AgentLeaseResourceKindV1::Symbol
                && l.request.resource_ref.starts_with(UNIT_SCHEME)
        })
        .filter(|l| {
            crate::core::agent_lease::release_shared(
                AgentLeaseResourceKindV1::Symbol,
                &l.request.resource_ref,
                agent_id,
                &l.lease_ref,
            )
            .unwrap_or(false)
        })
        .count()
}

// ─── The edit gate ───────────────────────────────────────────────────────────

/// Outcome of [`gate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Nothing held by another agent (or claims are off).
    Clear,
    /// Write, then show this warning.
    Warn(String),
    /// Do not write; this is the error.
    Refuse(String),
}

/// Checks an edit of `path` by `agent_id` against other agents' claims and,
/// unless refused, claims the units it touches.
#[must_use]
pub fn gate(project_root: &str, agent_id: &str, path: &Path, before: &str, after: &str) -> Gate {
    gate_with(mode(), project_root, agent_id, path, before, after)
}

/// [`gate`] with an explicit mode.
#[must_use]
pub fn gate_with(
    mode: ClaimMode,
    project_root: &str,
    agent_id: &str,
    path: &Path,
    before: &str,
    after: &str,
) -> Gate {
    if mode == ClaimMode::Off || project_root.is_empty() || agent_id.is_empty() {
        return Gate::Clear;
    }
    let Some(rel) = relative_path(project_root, path) else {
        return Gate::Clear;
    };
    let scope = repo_scope(project_root);
    let (names, spans) = edit_footprint(&rel, before, after);
    let leases = crate::core::agent_lease::active_shared().unwrap_or_default();
    let found = conflicts_in(
        leases,
        project_root,
        &scope,
        agent_id,
        &rel,
        before,
        &names,
        &spans,
    );
    if !found.is_empty() && mode == ClaimMode::Strict {
        return Gate::Refuse(render_conflicts(&found, true));
    }
    let _ = claim(&scope, agent_id, &rel, &names);
    if found.is_empty() {
        Gate::Clear
    } else {
        Gate::Warn(render_conflicts(&found, false))
    }
}

// ─── Snapshot for planners ───────────────────────────────────────────────────

/// One claim of the `leanctx.claims.v1` snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimEntry {
    pub holder: String,
    pub path: String,
    pub unit: String,
    pub expires_ms: Option<u64>,
}

/// The `leanctx.claims.v1` snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimsSnapshot {
    pub schema: String,
    pub claims: Vec<ClaimEntry>,
}

/// Active unit claims and path leases of the repository at `project_root`.
#[must_use]
pub fn snapshot(project_root: &str) -> ClaimsSnapshot {
    let scope = repo_scope(project_root);
    let leases = crate::core::agent_lease::active_shared().unwrap_or_default();
    let mut claims: Vec<ClaimEntry> = leases
        .into_iter()
        .filter_map(|lease| {
            let (path, unit) = match lease.request.resource_kind {
                AgentLeaseResourceKindV1::Symbol => {
                    let (s, path, unit) = parse_unit_resource(&lease.request.resource_ref)?;
                    (s == scope).then_some((path, unit))?
                }
                AgentLeaseResourceKindV1::Path => {
                    let path = crate::core::agent_lease::project_path_of(
                        project_root,
                        &lease.request.resource_ref,
                    )?;
                    (decode(&path).unwrap_or(path), WHOLE_FILE.to_string())
                }
                AgentLeaseResourceKindV1::WorkGraphNode => return None,
            };
            Some(ClaimEntry {
                holder: lease.request.owner_agent_id,
                path,
                unit,
                expires_ms: Some(lease.expires_at_epoch_ms),
            })
        })
        .collect();
    claims.sort_by(|a, b| (&a.path, &a.unit, &a.holder).cmp(&(&b.path, &b.unit, &b.holder)));
    ClaimsSnapshot {
        schema: CLAIMS_SCHEMA.to_string(),
        claims,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(owner: &str, kind: AgentLeaseResourceKindV1, resource: &str) -> AgentLeaseV1 {
        AgentLeaseV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_ref: format!("lease:{owner}"),
            request: AgentLeaseRequestV1 {
                schema_version: AGENT_LEASE_SCHEMA_VERSION,
                lease_request_ref: "lease-request:x".into(),
                resource_kind: kind,
                resource_ref: resource.into(),
                owner_agent_id: owner.into(),
                duration_ms: CLAIM_TTL_MS,
            },
            expires_at_epoch_ms: u64::MAX,
        }
    }

    const RUST: &str =
        "fn alpha() {\n    let a = 1;\n    a\n}\n\nfn beta() {\n    let b = 2;\n    b\n}\n";

    fn unit(scope: &str, rel: &str, name: &str) -> String {
        unit_resource(scope, rel, name).expect("bounded")
    }

    #[test]
    fn second_agent_on_the_same_unit_is_warned_refused_then_clear() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let file = dir.path().join("x.rs");
        let a = format!("agent-a-{}", std::process::id());
        let b = format!("agent-b-{}", std::process::id());
        let beta = RUST.replace("let b = 2;", "let b = 3;");
        let alpha = RUST.replace("let a = 1;", "let a = 5;");

        assert_eq!(
            gate_with(ClaimMode::Warn, &root, &a, &file, RUST, &beta),
            Gate::Clear
        );
        // Another unit of the same file: no conflict.
        assert_eq!(
            gate_with(ClaimMode::Warn, &root, &b, &file, RUST, &alpha),
            Gate::Clear
        );
        match gate_with(ClaimMode::Strict, &root, &b, &file, RUST, &beta) {
            Gate::Refuse(text) => assert!(text.contains(&a) && text.contains("`beta`"), "{text}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        match gate_with(ClaimMode::Warn, &root, &b, &file, RUST, &beta) {
            Gate::Warn(text) => assert!(text.contains(&a), "{text}"),
            other => panic!("expected a warning, got {other:?}"),
        }
        let held: Vec<(String, String)> = snapshot(&root)
            .claims
            .into_iter()
            .map(|c| (c.holder, c.unit))
            .collect();
        assert!(held.contains(&(a.clone(), "beta".to_string())), "{held:?}");
        assert!(held.contains(&(b.clone(), "alpha".to_string())), "{held:?}");

        assert_eq!(release_agent(&a), 1);
        assert_eq!(
            gate_with(ClaimMode::Strict, &root, &b, &file, RUST, &beta),
            Gate::Clear
        );
        assert_eq!(release_agent(&b), 2);
        assert!(snapshot(&root).claims.is_empty());
    }

    #[test]
    fn unit_resource_round_trips_and_is_bounded() {
        let r = unit("0123456789abcdef", "src/a b.rs", "Foo::bar");
        assert_eq!(r, "project-unit:0123456789abcdef/src/a%20b.rs#Foo::bar");
        assert_eq!(
            parse_unit_resource(&r),
            Some((
                "0123456789abcdef".into(),
                "src/a b.rs".into(),
                "Foo::bar".into()
            ))
        );
        assert!(unit_resource("s", &"x".repeat(300), "f").is_none());
        assert_eq!(
            parse_unit_resource(&unit("s", "a.rs", "a#b")).unwrap().2,
            "a#b"
        );
    }

    #[test]
    fn edit_touches_only_the_innermost_changed_unit() {
        let after = RUST.replace("let b = 2;", "let b = 3;");
        let (names, spans) = edit_footprint("x.rs", RUST, &after);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["beta".to_string()]
        );
        assert_eq!(spans, vec![(7, 7)]);
    }

    #[test]
    fn insertion_between_units_touches_only_the_new_unit() {
        let after = RUST.replace("}\n\nfn beta", "}\n\nfn gamma() {}\n\nfn beta");
        let (names, _) = edit_footprint("x.rs", RUST, &after);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["gamma".to_string()]
        );
    }

    #[test]
    fn markdown_sections_nest_and_skip_fences() {
        let md = "# A\nx\n## B\n```\n# not\n```\ny\n# C\nz\n";
        let spans: Vec<(String, usize, usize)> = markdown_sections(md)
            .into_iter()
            .map(|u| (u.name, u.start, u.end))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("A".to_string(), 1, 7),
                ("B".to_string(), 3, 7),
                ("C".to_string(), 8, 9)
            ]
        );
        let after = md.replace("y\n", "y2\n");
        let (names, _) = edit_footprint("doc.md", md, &after);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec!["B".to_string()]);
    }

    #[test]
    fn conflicts_name_other_holders_only() {
        let scope = "0123456789abcdef";
        let after = RUST.replace("let b = 2;", "let b = 3;");
        let (names, spans) = edit_footprint("x.rs", RUST, &after);
        let symbol = AgentLeaseResourceKindV1::Symbol;
        let leases = vec![
            lease("me", symbol, &unit(scope, "x.rs", "beta")),
            lease("other", symbol, &unit(scope, "x.rs", "beta")),
            lease("third", symbol, &unit(scope, "x.rs", "alpha")),
            lease("fourth", symbol, &unit("ffffffffffffffff", "x.rs", "beta")),
            lease("whole", symbol, &unit(scope, "x.rs", WHOLE_FILE)),
        ];
        let found = conflicts_in(
            leases,
            "/nonexistent",
            scope,
            "me",
            "x.rs",
            RUST,
            &names,
            &spans,
        );
        let holders: Vec<(&str, &str)> = found
            .iter()
            .map(|c| (c.holder.as_str(), c.unit.as_str()))
            .collect();
        assert_eq!(holders, vec![("other", "beta"), ("whole", "*")]);
        let text = render_conflicts(&found, true);
        assert!(text.contains("claimed by other"), "{text}");
        assert!(text.starts_with("ERROR"), "{text}");
    }

    #[test]
    fn path_lease_above_the_file_conflicts() {
        let root = "/nonexistent/claims-root";
        let dir = crate::core::agent_lease::project_path_resource(root, "src").unwrap();
        let leases = vec![lease("other", AgentLeaseResourceKindV1::Path, &dir)];
        let names = BTreeSet::new();
        let found = conflicts_in(leases.clone(), root, "s", "me", "src/x.rs", "", &names, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].unit, WHOLE_FILE);
        let none = conflicts_in(leases, root, "s", "me", "srcx/x.rs", "", &names, &[]);
        assert!(none.is_empty());
    }

    #[test]
    fn off_mode_and_outside_paths_are_clear() {
        assert_eq!(
            gate_with(ClaimMode::Off, "/r", "a", Path::new("/r/x.rs"), "", "x"),
            Gate::Clear
        );
        assert_eq!(relative_path("/r", Path::new("/elsewhere/x.rs")), None);
        assert_eq!(
            relative_path("/r", Path::new("/r/src/x.rs")).as_deref(),
            Some("src/x.rs")
        );
    }
}
