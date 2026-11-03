// SPDX-License-Identifier: Apache-2.0

#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(windows, allow(dead_code))] // consumers: a cfg(not(windows)) test and server::isolation_fixture
pub(in crate::server) async fn dispatch_and_post_process(
    server: &LeanCtxServer,
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
    minimal: bool,
    config: std::sync::Arc<lean_ctx::core::config::Config>,
    machine_readable: bool,
    auto_context: Option<String>,
    throttle_warning: Option<String>,
    args_fp: String,
    decision_context: Option<lean_ctx::core::decision_loop_runtime::TaskContext>,
) -> Result<CallToolResult, ErrorData> {
    let raw = dispatch_primitive(server, name, args, minimal, &args_fp).await?;
    let mut processed = reversible_post_process(
        server,
        name,
        args,
        minimal,
        config,
        machine_readable,
        auto_context,
        throttle_warning,
        decision_context,
        raw,
    )
    .await?;
    let _ = record_context_ir(server, &processed).await;
    record_ledger(server, &processed).await?;
    let checkpoint = processed.checkpoint.take();
    let _ = record_checkpoint(server, checkpoint, &mut processed.result).await;
    Ok(processed.result)
}
