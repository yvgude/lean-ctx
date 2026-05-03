#[tokio::main]
async fn main() {
    lean_ctx::core::logging::init_mcp_logging();

    if let Err(e) = lean_ctx::cloud_server::run().await {
        tracing::error!("Cloud server error: {e}");
        std::process::exit(1);
    }
}
