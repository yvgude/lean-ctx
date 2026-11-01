// SPDX-License-Identifier: Apache-2.0

//! Generic text compressors.
//!
//! The compressors live in the `lean-ctx-compress` crate and are re-exported
//! here unchanged. This facade adds what needs the engine: compression steered
//! by the active task intent.

pub use lean_ctx_compress::compressor::*;

/// Compresses content using the active task intent to preserve task-relevant sections.
pub fn task_aware_compress(
    content: &str,
    ext: Option<&str>,
    intent: &crate::core::intent_engine::StructuredIntent,
) -> String {
    use crate::core::intent_engine::{IntentScope, TaskType};

    let budget_ratio = match intent.scope {
        IntentScope::SingleFile => 0.7,
        IntentScope::MultiFile => 0.5,
        IntentScope::CrossModule => 0.35,
        IntentScope::ProjectWide => 0.25,
    };

    match intent.task_type {
        TaskType::FixBug | TaskType::Debug => {
            let filtered = crate::core::task_relevance::information_bottleneck_filter_typed(
                content,
                &intent.keywords,
                budget_ratio,
                Some(intent.task_type),
                &[],
            );
            safeguard_ratio(content, &filtered)
        }
        TaskType::Refactor | TaskType::Review => {
            let cleaned = lightweight_cleanup(content);
            let filtered = crate::core::task_relevance::information_bottleneck_filter_typed(
                &cleaned,
                &intent.keywords,
                budget_ratio.max(0.5),
                Some(intent.task_type),
                &[],
            );
            safeguard_ratio(content, &filtered)
        }
        TaskType::Generate | TaskType::Test => {
            let compressed = aggressive_compress(content, ext);
            safeguard_ratio(content, &compressed)
        }
        TaskType::Explore | TaskType::Config | TaskType::Deploy => {
            let cleaned = lightweight_cleanup(content);
            safeguard_ratio(content, &cleaned)
        }
    }
}
