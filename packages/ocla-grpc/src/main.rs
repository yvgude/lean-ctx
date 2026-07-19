//! Loopback-only OCLA gRPC verifier executable.

use std::process::ExitCode;

use lean_ctx_ocla_grpc::{parse_loopback_addr, serve};
use tokio::net::TcpListener;

const DEFAULT_LISTEN: &str = "127.0.0.1:50051";

#[tokio::main]
async fn main() -> ExitCode {
    if run().await.is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("OCLA gRPC server rejected");
        ExitCode::from(2)
    }
}

async fn run() -> Result<(), ()> {
    let mut args = std::env::args().skip(1);
    let argument = args.next();
    let listen = argument.as_deref().map_or(Ok(DEFAULT_LISTEN), |value| {
        value.strip_prefix("--listen=").ok_or(())
    })?;
    if args.next().is_some() {
        return Err(());
    }
    let address = parse_loopback_addr(listen).map_err(|_| ())?;
    let listener = TcpListener::bind(address).await.map_err(|_| ())?;
    serve(listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| ())
}
