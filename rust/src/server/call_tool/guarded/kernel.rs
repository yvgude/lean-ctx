// SPDX-License-Identifier: Apache-2.0

//! Admission-owned Community context planning for actual kernel consumers.

use std::{sync::Arc, time::Duration};

use super::{LeanCtxServer, PreparedMcpCall};

#[cfg(test)]
mod tests;
use crate::core::{
    context_kernel::bridge::runtime::{KernelPlanningHandoff, PreparedKernelContext},
    execution_lifecycle::{StageDisposition, TaskContext},
};

#[derive(Clone, Copy)]
enum Budget {
    Compose,
    Semantic,
    BatchRead,
}

struct PlanningInputs {
    query: String,
    root: String,
    budget: Budget,
    explicit_mode: Option<String>,
}

/// Only actual supplement consumers participate. Single reads and shell do not
/// currently call the legacy enrichment helper from their MCP handlers.
async fn inputs(
    server: &LeanCtxServer,
    call: &PreparedMcpCall,
    context: &TaskContext,
) -> anyhow::Result<Option<PlanningInputs>> {
    let (name, args) = crate::tools::registered::ctx_call::resolve(&call.name, call.args.as_ref())
        .map_err(|error| anyhow::anyhow!("invalid tool envelope: {error}"))?;
    let Some(args) = args.as_ref() else {
        return Ok(None);
    };
    let string = |key| args.get(key).and_then(serde_json::Value::as_str);
    let (query, budget, explicit_mode) = match name.as_str() {
        "ctx_compose" => (string("task").map(str::to_owned), Budget::Compose, None),
        "ctx_search" if crate::tools::registered::ctx_search::uses_kernel_context(args) => {
            (string("query").map(str::to_owned), Budget::Semantic, None)
        }
        "ctx_multi_read" | "ctx_read"
            if args.get("paths").is_some_and(serde_json::Value::is_array) =>
        {
            anyhow::ensure!(
                string("mode") != Some("raw")
                    && args.get("raw").and_then(serde_json::Value::as_bool) != Some(true),
                "raw output excludes context supplementation"
            );
            (
                context.planning_query().map(str::to_owned),
                Budget::BatchRead,
                string("mode").map(str::to_owned),
            )
        }
        _ => return Ok(None),
    };
    let Some(query) = query else { return Ok(None) };
    anyhow::ensure!(query.len() <= 65_536, "kernel query exceeds planning bound");
    anyhow::ensure!(
        explicit_mode.as_ref().is_none_or(|mode| mode.len() <= 256),
        "kernel mode exceeds planning bound"
    );
    let root = if let Some(path) = args
        .get("path")
        .filter(|_| !matches!(budget, Budget::BatchRead))
    {
        let path = path
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path must be a string"))?;
        server
            .resolve_path(path)
            .await
            .map_err(anyhow::Error::msg)?
    } else {
        let Some(root) = context
            .runtime
            .project_root
            .as_ref()
            .filter(|root| !root.is_empty())
        else {
            return Ok(None);
        };
        server
            .resolve_path(root)
            .await
            .map_err(anyhow::Error::msg)?
    };
    Ok(Some(PlanningInputs {
        query,
        root,
        budget,
        explicit_mode,
    }))
}

pub(super) async fn plan(
    server: &LeanCtxServer,
    call: &PreparedMcpCall,
    context: &TaskContext,
) -> (StageDisposition, KernelPlanningHandoff) {
    let inputs =
        match tokio::time::timeout(Duration::from_secs(2), inputs(server, call, context)).await {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        };
    let inputs = match inputs {
        Ok(Some(inputs)) => inputs,
        Ok(None) => {
            return (
                StageDisposition::Skipped("no canonical autopilot decision"),
                KernelPlanningHandoff::Suppressed,
            );
        }
        Err(error) => {
            tracing::debug!(%error, "kernel planning input unavailable");
            return (
                StageDisposition::Skipped("kernel planning input unavailable"),
                KernelPlanningHandoff::Suppressed,
            );
        }
    };
    let envelope = context.envelope.clone();
    let task_class = context.autopilot_task_class();
    #[cfg(test)]
    context
        .autopilot_planning_attempts()
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut worker = tokio::task::spawn_blocking(move || {
        let PlanningInputs {
            query,
            root,
            budget,
            explicit_mode,
        } = inputs;
        let budget = match budget {
            Budget::Compose => crate::tools::ctx_compose::kernel_supplement_budget(&root),
            Budget::Semantic => 100,
            Budget::BatchRead => crate::core::context_kernel::activation::supplement_budget(
                &crate::core::context_kernel::activation::load_config(&root),
            ),
        };
        PreparedKernelContext::plan_for_envelope(
            envelope,
            query,
            root,
            budget,
            task_class,
            explicit_mode,
        )
    });
    // Providers are read-only here. Timeout bounds MCP latency; a started blocking
    // read may finish later, but its unadmitted result is never attached or used.
    match tokio::time::timeout(Duration::from_secs(5), &mut worker).await {
        Ok(Ok(Ok(prepared))) => {
            let prepared = Arc::new(prepared);
            if context.attach_autopilot(prepared.clone()).is_ok() {
                return (
                    StageDisposition::Applied,
                    KernelPlanningHandoff::Prepared(prepared),
                );
            }
        }
        _ => worker.abort(),
    }
    (
        StageDisposition::Skipped("kernel planning unavailable"),
        KernelPlanningHandoff::Suppressed,
    )
}
