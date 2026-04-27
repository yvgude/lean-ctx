#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    if let Err(e) = lean_ctx::cloud_server::run().await {
        tracing::error!("Cloud server error: {e}");
        std::process::exit(1);
    }
}
