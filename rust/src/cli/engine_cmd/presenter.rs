// SPDX-License-Identifier: Apache-2.0

//! `context-materialize-sources-v2`, `context-presented-use` and
//! `context-presenter-revoke`: the operations that issue, use and revoke a
//! presenter capability.
//!
//! A capability a host already holds reaches the Engine only through
//! `--presenter-capability-file` — never through an argument value, an
//! environment variable or the request JSON. It is printed in exactly one
//! case: the response of a successful `issue`. No error names it.
//!
//! `context-presented-use` (wire types: `lean_ctx_protocol`'s
//! `engine_presented_use`) is the one-shot consumer of a presented lineage:
//! the same register and recovery functions the proxy and `ctx_expand` use
//! (`delivered::lineage_of`, `Lineage::still_admitted`,
//! `store_recovery_with_lineage`, `retrieve_recovery_checked`), run for the
//! holder of the capability in the file. Its one addition is the source
//! constraint (`authorized_source_ids`): a caller in front of the Engine that
//! has authorized this request for certain sources — the suite — names them,
//! and a lineage of any other source set is unknown to the request BEFORE
//! anything is checked, stored or returned for it. A local host without such
//! a caller sends no constraint; capability and lineage alone decide then.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::{
    EngineContextSourceMaterializationRequestV2, EnginePresentedUseRequestV1,
    EnginePresentedUseResponseV1, EnginePresentedUseV1, EnginePresenterRevocationRequestV1,
    EnginePresenterRevocationResponseV1, MAX_ENGINE_PRESENTED_USE_REQUEST_BYTES,
    MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES, ProtocolReference, SourceId,
};

use super::{EngineCliError, decode_request, read_plan_request, validate_header};
use crate::core::archive::presented::{self, PresenterCapability};
use crate::core::archive::{self, RecoveryId, delivered};
use crate::core::engine_interface::context_plan::presenter::{CAPABILITY_REFUSED, materialize_v2};

pub(super) const MATERIALIZE_V2: &str = "context-materialize-sources-v2";
pub(super) const REVOKE: &str = "context-presenter-revoke";
pub(super) const USE: &str = "context-presented-use";
const UNREADABLE_REQUEST: &str = "invalid_source_materialization_request";
/// A use request this operation cannot read. The bare `invalid_request` is
/// what an Engine WITHOUT the operation answers.
const UNREADABLE_USE_REQUEST: &str = "invalid_presented_use_request";

/// The v2 envelope around a v1 request of at most the v1 bound.
const MAX_V2_REQUEST_BYTES: usize = MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES + 256;
const MAX_REVOCATION_REQUEST_BYTES: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PresenterOperation {
    Materialize,
    Revoke,
    Use,
}

struct PresenterCliArgs {
    operation: PresenterOperation,
    project_root: PathBuf,
    json_file: PathBuf,
    capability_file: Option<PathBuf>,
}

const UNKNOWN_LINEAGE: &str = "lineage.unknown";

/// The source ids a caller in front of the Engine has authorized for one use
/// request (the request's `authorized_source_ids`, validated by the protocol
/// type): sorted, distinct, content-free.
///
/// A use is answered only for a lineage whose source ids are EXACTLY this
/// set. Equality, not inclusion: a lineage with fewer sources belongs to
/// another text than the one the sender authorized, and a lineage with more
/// derives from a source nobody authorized for this request. Neither is the
/// thing that was authorized, so neither is checked, stored or handed out.
struct SourceConstraint(Vec<String>);

impl SourceConstraint {
    /// The set of `ids`, whatever their order.
    fn of(ids: impl IntoIterator<Item = String>) -> Self {
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        Self(ids)
    }
}

/// The lineage on record for `text`, with its source ids: a lineage of
/// caller-supplied sources only and, under a constraint, of exactly the
/// constraint's sources. Any other text is unknown to the request — the one
/// answer, whether nothing is on record or something else is — and that is
/// decided here, before the lineage is checked and before any mutation.
fn known_lineage(
    text: &str,
    constraint: Option<&SourceConstraint>,
) -> Option<(delivered::Lineage, Vec<String>)> {
    let lineage = delivered::lineage_of(&delivered::digest(text))?;
    let source_ids = lineage.presented_source_ids()?;
    constraint
        .is_none_or(|permitted| permitted.0 == source_ids)
        .then_some((lineage, source_ids))
}

/// Answer `operation` for the holder of `capability`.
///
/// Refused as a capability (`Err`, the one refusal) unless the capability is
/// an issued one that is neither expired nor revoked. Everything after that
/// is the Engine's ordinary lineage authority, asked for this holder:
/// project, caller and role of the request, the current policy identity, and
/// a presentation by exactly this presenter within the validity. Only a
/// lineage of caller-supplied sources is served here; a text that (also)
/// derives from project files is unknown to this operation, and so is, under
/// a `constraint`, a lineage of any other source set than the constraint's.
///
/// Order in every mode: capability, then the lineage and its source set
/// ([`known_lineage`]), then current authority, and only then the mutation
/// (`retain` stores) or the disclosure (`expand` returns text). A refused
/// `retain` has stored nothing.
fn use_presented(
    root: &Path,
    capability: &PresenterCapability,
    constraint: Option<&SourceConstraint>,
    operation: EnginePresentedUseV1,
) -> Result<EnginePresentedUseResponseV1, &'static str> {
    presented::issued_lifetime_end(capability).ok_or(CAPABILITY_REFUSED)?;
    let presenter_ref = capability.reference();
    let project = root.to_str().ok_or("unsafe_root")?;
    let refused = |reason: String| EnginePresentedUseResponseV1 {
        schema_version: 1,
        presenter_ref: presenter_ref.clone(),
        usable: false,
        reason: Some(reason),
        source_ids: Vec::new(),
        recovery_id: None,
        content: None,
    };
    let answer = presented::with_presenter(Some(capability.clone()), || {
        crate::core::policy::runtime::with_project_source_view(project, || match operation {
            EnginePresentedUseV1::Check { content } => {
                let Some((lineage, source_ids)) = known_lineage(&content, constraint) else {
                    return refused(UNKNOWN_LINEAGE.to_owned());
                };
                match lineage.still_admitted() {
                    Ok(()) => EnginePresentedUseResponseV1 {
                        schema_version: 1,
                        presenter_ref: presenter_ref.clone(),
                        usable: true,
                        reason: None,
                        source_ids,
                        recovery_id: None,
                        content: None,
                    },
                    Err(refusal) => refused(refusal.to_string()),
                }
            }
            EnginePresentedUseV1::Retain { content } => {
                // Before the store is touched: a lineage of other sources than
                // the permitted ones is refused here and leaves no copy.
                let Some((lineage, source_ids)) = known_lineage(&content, constraint) else {
                    return refused(UNKNOWN_LINEAGE.to_owned());
                };
                // Checks the lineage now and that the text may enter a store.
                match archive::store_recovery_with_lineage(&content, &lineage) {
                    Ok(id) => EnginePresentedUseResponseV1 {
                        schema_version: 1,
                        presenter_ref: presenter_ref.clone(),
                        usable: true,
                        reason: None,
                        source_ids,
                        recovery_id: Some(id.as_str().to_owned()),
                        content: None,
                    },
                    Err(refusal) => refused(refusal.to_string()),
                }
            }
            EnginePresentedUseV1::Expand { recovery_id } => {
                let Some(id) = RecoveryId::parse(&recovery_id) else {
                    return refused(archive::RecoveryRefusal::Missing.to_string());
                };
                // Before the copy is read: a copy bound to project files, or
                // to other sources than the permitted ones, is not resolved.
                let permitted = archive::recovery_lineage(&id)
                    .and_then(|lineage| lineage.presented_source_ids())
                    .is_some_and(|ids| constraint.is_none_or(|permitted| permitted.0 == ids));
                if !permitted {
                    return refused(UNKNOWN_LINEAGE.to_owned());
                }
                // Re-checks every source under current authority and registers
                // the released text as delivered under its lineage.
                let content = match archive::retrieve_recovery_checked(&id) {
                    Ok(content) => content,
                    Err(refusal) => return refused(refusal.to_string()),
                };
                // A copy bound to project files is not a presented lineage,
                // and a copy of other sources than the permitted ones is not
                // this request's: its text is not handed out here.
                let Some((_, source_ids)) = known_lineage(&content, constraint) else {
                    return refused(UNKNOWN_LINEAGE.to_owned());
                };
                EnginePresentedUseResponseV1 {
                    schema_version: 1,
                    presenter_ref: presenter_ref.clone(),
                    usable: true,
                    reason: None,
                    source_ids,
                    recovery_id: Some(id.as_str().to_owned()),
                    content: Some(content),
                }
            }
        })
    })
    .map_err(|_| "source_policy_view_unavailable")?;
    // The capability is never part of an answer, whatever the text holds.
    let wire = capability.expose();
    if answer
        .content
        .as_deref()
        .is_some_and(|content| content.contains(wire.as_str()))
    {
        return Err("internal");
    }
    Ok(answer)
}

fn parse(args: &[String]) -> Result<PresenterCliArgs, EngineCliError> {
    let operation = match args.first().map(String::as_str) {
        Some(MATERIALIZE_V2) => PresenterOperation::Materialize,
        Some(REVOKE) => PresenterOperation::Revoke,
        Some(USE) => PresenterOperation::Use,
        _ => return Err(EngineCliError::Usage),
    };
    let (mut project_root, mut json_file, mut capability_file) = (None, None, None);
    let mut index = 1;
    while index < args.len() {
        let slot = match args[index].as_str() {
            "--project-root" => &mut project_root,
            "--json-file" => &mut json_file,
            "--presenter-capability-file" => &mut capability_file,
            _ => return Err(EngineCliError::Usage),
        };
        index += 1;
        let value = args.get(index).ok_or(EngineCliError::Usage)?;
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(EngineCliError::Usage);
        }
        index += 1;
    }
    Ok(PresenterCliArgs {
        operation,
        project_root: project_root.ok_or(EngineCliError::Usage)?,
        json_file: json_file.ok_or(EngineCliError::Usage)?,
        capability_file,
    })
}

pub(super) fn run(args: &[String]) -> Result<String, EngineCliError> {
    let cli = parse(args)?;
    let shown = match cli.capability_file.as_deref() {
        Some(path) => Some(
            presented::read_capability_file(path)
                .ok_or(EngineCliError::Host(CAPABILITY_REFUSED))?,
        ),
        None => None,
    };
    if cli.operation == PresenterOperation::Use {
        let unreadable = |error| match error {
            EngineCliError::Request => EngineCliError::Host(UNREADABLE_USE_REQUEST),
            other => other,
        };
        let bytes = read_plan_request(&cli.json_file, MAX_ENGINE_PRESENTED_USE_REQUEST_BYTES)
            .map_err(unreadable)?;
        let request: EnginePresentedUseRequestV1 = decode_request(&bytes).map_err(unreadable)?;
        if request.schema_version != 1 {
            return Err(EngineCliError::UnsupportedSchema);
        }
        // A text or recovery id out of form, and a constraint that is not a
        // bounded set of distinct ids, are an unreadable request: a
        // constraint is never narrowed, widened or ignored.
        request
            .validate_payload()
            .map_err(|_| EngineCliError::Host(UNREADABLE_USE_REQUEST))?;
        let root = crate::core::engine_interface::bind_transport_root(&cli.project_root)
            .map_err(EngineCliError::Engine)?;
        let capability = shown.ok_or(EngineCliError::Host(CAPABILITY_REFUSED))?;
        let constraint = request
            .authorized_source_ids
            .map(|ids| SourceConstraint::of(ids.into_iter().map(SourceId::into_inner)));
        let response = use_presented(&root, &capability, constraint.as_ref(), request.operation)
            .map_err(EngineCliError::Host)?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == PresenterOperation::Revoke {
        let request: EnginePresenterRevocationRequestV1 = decode_request(&read_plan_request(
            &cli.json_file,
            MAX_REVOCATION_REQUEST_BYTES,
        )?)?;
        if request.schema_version != 1 {
            return Err(EngineCliError::UnsupportedSchema);
        }
        crate::core::engine_interface::bind_transport_root(&cli.project_root)
            .map_err(EngineCliError::Engine)?;
        let capability = shown.ok_or(EngineCliError::Host(CAPABILITY_REFUSED))?;
        let response = EnginePresenterRevocationResponseV1 {
            schema_version: 1,
            presenter_ref: ProtocolReference::new(capability.reference())
                .map_err(|_| EngineCliError::Host("internal"))?,
            revoked: presented::revoke(&capability),
        };
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    // A request this operation cannot read has its own code. The bare
    // `invalid_request` is what an Engine WITHOUT this operation answers, and
    // a client tells the two apart by exactly that.
    let unreadable = |error| match error {
        EngineCliError::Request => EngineCliError::Host(UNREADABLE_REQUEST),
        other => other,
    };
    let request: EngineContextSourceMaterializationRequestV2 = decode_request(
        &read_plan_request(&cli.json_file, MAX_V2_REQUEST_BYTES).map_err(unreadable)?,
    )
    .map_err(unreadable)?;
    let planning = &request.materialization.source_plan.planning;
    validate_header(
        planning.schema_version,
        planning.transport_version,
        &planning.engine_interface_version,
    )?;
    let response =
        materialize_v2(&cli.project_root, &request, shown).map_err(EngineCliError::Host)?;
    serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capability has one way in: the file option. An argument value, a
    /// repeated option or an unknown option is a usage error, and a file
    /// that is not a usable capability file is the one capability refusal.
    #[test]
    fn a_capability_enters_through_the_file_option_only() {
        let argv = |extra: &[&str]| -> Vec<String> {
            [
                MATERIALIZE_V2,
                "--project-root",
                "/tmp/p",
                "--json-file",
                "-",
            ]
            .iter()
            .chain(extra)
            .map(|part| (*part).to_owned())
            .collect()
        };
        assert!(parse(&argv(&[])).is_ok_and(|cli| {
            cli.capability_file.is_none() && cli.operation == PresenterOperation::Materialize
        }));
        assert!(parse(&argv(&["--presenter-capability-file", "/tmp/c"])).is_ok());
        let literal = format!("lcp1_{}", "ab".repeat(32));
        for refused in [
            vec!["--presenter-capability", literal.as_str()],
            vec![literal.as_str()],
            vec!["--presenter-capability-file"],
            vec![
                "--presenter-capability-file",
                "/tmp/c",
                "--presenter-capability-file",
                "/tmp/d",
            ],
        ] {
            assert!(parse(&argv(&refused)).is_err(), "{refused:?}");
        }

        let directory = tempfile::tempdir().expect("directory");
        let missing = directory.path().join("absent");
        let mut with_file = argv(&["--presenter-capability-file"]);
        with_file.push(missing.to_string_lossy().into_owned());
        assert_eq!(
            run(&with_file).map_err(|error| error.code()),
            Err(CAPABILITY_REFUSED)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let open = directory.path().join("group-readable");
            std::fs::write(&open, &literal).expect("file");
            std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o640)).expect("mode");
            assert!(presented::read_capability_file(&open).is_none());
            std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o600)).expect("mode");
            assert!(presented::read_capability_file(&open).is_some());
        }
    }

    /// The one-shot consumer of a presented lineage. The holder of the
    /// capability a text was presented under keeps a recovery copy of it and
    /// gets the original back while its source holds; another capability, a
    /// capability nobody issued, a changed policy and a revoked capability
    /// get no text. Before, no one-shot operation reached the lineage and
    /// recovery path at all: a capability could be issued and renewed, and
    /// nothing could be asked with it.
    ///
    /// Under a source constraint the lineage must name exactly the permitted
    /// sources, and that is decided before the store is touched. Before,
    /// `retain` stored its copy first and the caller in front of the Engine
    /// compared the sources afterwards: a request it then refused had
    /// already left a copy behind.
    #[test]
    fn a_presented_text_is_expanded_for_its_holder_only_while_its_source_holds() {
        use crate::core::archive::presented::{PresentedBinding, record_presentation};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(config.join("config.toml"), "").expect("config");
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        let text = "## leanctx-source-v1\ninvoice ledger\n";
        let holder = presented::issue(3600).expect("issued").capability;
        let other = presented::issue(3600).expect("issued").capability;
        // Assembled at run time: the source holds no capability-shaped text.
        let unissued = PresenterCapability::parse(&format!("lcp1_{}", "0f".repeat(32)))
            .expect("capability form");

        // What a v2 materialization under `holder` leaves in the store: the
        // presentation of the source and the lineage of the delivered text.
        presented::with_presenter(Some(holder.clone()), || {
            crate::core::policy::runtime::with_project_source_view(
                root.to_str().expect("utf-8 root"),
                || {
                    let binding = PresentedBinding::admitted_now(
                        &root,
                        "ref-quartz",
                        "source-one",
                        "sha256:aa",
                    )
                    .expect("policy identity available");
                    assert!(record_presentation(&binding));
                    assert!(delivered::record_lineage(
                        &delivered::digest(text),
                        &delivered::Lineage::of_presented(vec![binding]),
                    ));
                },
            )
        })
        .expect("source view");

        let ask = |capability: &PresenterCapability, operation: EnginePresentedUseV1| {
            use_presented(&root, capability, None, operation)
                .map(|answer| (answer.usable, answer.reason, answer.content))
        };
        let only = |ids: &[&str]| SourceConstraint::of(ids.iter().map(|id| (*id).to_owned()));
        let retain = |permitted: &SourceConstraint| {
            use_presented(
                &root,
                &holder,
                Some(permitted),
                EnginePresentedUseV1::Retain {
                    content: text.to_owned(),
                },
            )
            .expect("the holder is asked")
        };
        let check = || EnginePresentedUseV1::Check {
            content: text.to_owned(),
        };
        let exact = only(&["source-one"]);
        let kept = retain(&exact);
        assert!(kept.usable && kept.content.is_none());
        assert_eq!(kept.source_ids, ["source-one"]);
        let id = kept.recovery_id.expect("a recovery copy is kept");
        let expand = || EnginePresentedUseV1::Expand {
            recovery_id: id.clone(),
        };
        let not_presented = |prefix: &str| Some(format!("{prefix}.source_not_presented"));

        for (case, answer, expected) in [
            (
                "the holder checks its text",
                ask(&holder, check()),
                Ok((true, None, None)),
            ),
            (
                "the holder gets the original back",
                ask(&holder, expand()),
                Ok((true, None, Some(text.to_owned()))),
            ),
            (
                "another capability checks the same text",
                ask(&other, check()),
                Ok((false, not_presented("lineage"), None)),
            ),
            (
                "another capability asks for the copy",
                ask(&other, expand()),
                Ok((false, not_presented("recovery"), None)),
            ),
            (
                "a text nothing was recorded for",
                ask(
                    &holder,
                    EnginePresentedUseV1::Check {
                        content: "never delivered".to_owned(),
                    },
                ),
                Ok((false, Some(UNKNOWN_LINEAGE.to_owned()), None)),
            ),
            (
                "a capability nobody issued",
                ask(&unissued, expand()),
                Err(CAPABILITY_REFUSED),
            ),
        ] {
            assert_eq!(answer, expected, "{case}");
        }

        // The source constraint, shown on an empty slot: the copy is removed,
        // a `retain` for another source set than the lineage's is unknown and
        // leaves the slot empty, and the permitted one fills it again under
        // the same id.
        archive::remove_files(&id);
        // A slot without a copy has no lineage on record, and `expand` decides
        // on the lineage BEFORE it reads a copy: the answer is the one a copy
        // of other sources gets, so an id is no oracle for what exists.
        let missing = Ok((false, Some(UNKNOWN_LINEAGE.to_owned()), None));
        assert_eq!(ask(&holder, expand()), missing, "the copy is gone");
        for (case, other_set) in [
            ("another source", only(&["source-two"])),
            ("one source more", only(&["source-one", "source-two"])),
        ] {
            let refused = retain(&other_set);
            assert!(!refused.usable && refused.recovery_id.is_none(), "{case}");
            assert_eq!(refused.reason.as_deref(), Some(UNKNOWN_LINEAGE), "{case}");
            assert_eq!(ask(&holder, expand()), missing, "{case}: nothing is stored");
        }
        assert_eq!(retain(&exact).recovery_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            ask(&holder, expand()),
            Ok((true, None, Some(text.to_owned()))),
            "the permitted set keeps the copy"
        );

        // The policy the source was admitted under is withdrawn: the copy is
        // not served under the new one, to the holder either.
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        assert_eq!(
            ask(&holder, expand()),
            Ok((
                false,
                Some("recovery.source_no_longer_admitted".to_owned()),
                None
            ))
        );
        assert!(presented::revoke(&holder));
        assert_eq!(ask(&holder, expand()), Err(CAPABILITY_REFUSED), "revoked");
    }
}
