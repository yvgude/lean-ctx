//! Generates the checked-in OCLA gRPC bindings from the public protocol.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../contracts/ocla/v1/ocla.proto";
    let mut prost = tonic_prost_build::Config::new();
    prost.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure().compile_with_config(prost, &[proto], &["../../contracts"])?;
    println!("cargo:rerun-if-changed={proto}");
    Ok(())
}
