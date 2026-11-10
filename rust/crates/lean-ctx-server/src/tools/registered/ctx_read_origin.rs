// SPDX-License-Identifier: Apache-2.0
//! Outside text inside a `ctx_read` reply (pz21 §1.6, §1.8).

use lean_ctx::core::context_admission::{capture, origin};

/// The cross-source hint lines, admitted as outside text: each hint names an
/// issue, a pull request or another provider item, so the lines are a
/// marked part of the reply. The release point fences exactly that part,
/// and the project file body stays unfenced (line refinement, §1.5).
pub(super) fn admitted_hints(
    hints: &[lean_ctx::core::cross_source_hints::CrossSourceHint],
) -> String {
    let formatted = lean_ctx::core::cross_source_hints::format_hints(hints);
    let Some(first) = hints.first() else {
        return formatted;
    };
    // The part is fenced under the first hint's origin.
    let class = origin::provider_uri_class(&first.source_uri);
    capture::external_span(&class, || {
        lean_ctx::core::context_admission::admit_context_source(&formatted, &class)
            .ok()
            .map(|admission| admission.text)
    })
    .unwrap_or_default()
}

/// A read of a file this session downloaded through a matched command is
/// outside text: noted in the call's ledger, so the read is fenced with
/// its `file:` reference.
pub(super) fn note_downloaded_read(path: &str, project_root: &str) {
    let root = std::path::Path::new(project_root);
    let Some(download) = origin::download_origin(root, root, std::path::Path::new(path)) else {
        return;
    };
    let Some(input) = download.input_ref() else {
        return;
    };
    if origin::session_taint().contains(&input) {
        capture::note_origin(&origin::SourceClass::External(download));
    }
}
