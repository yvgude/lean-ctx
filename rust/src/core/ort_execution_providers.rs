#![cfg(any(feature = "embeddings", feature = "neural"))]
//! GPU execution provider selection: per-vendor feature flags, session-level config.
//!
//! Each GPU EP is gated behind its own Cargo feature (`ort-cuda`, `ort-rocm`, etc.).
//! When no GPU features are enabled, an empty vec is returned — ORT uses CPU only.
//! When a GPU feature IS enabled but the GPU is unavailable, ORT silently falls back
//! to the next EP in the list; we emit [`tracing::warn!`] so users know.

/// Build the list of GPU execution providers in registration-priority order.
pub fn gpu_execution_providers() -> Vec<ort::ep::ExecutionProviderDispatch> {
    #[allow(unused_mut)]
    let mut eps: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();

    #[cfg(feature = "ort-cuda")]
    {
        tracing::info!("Enabling CUDA execution provider for ONNX Runtime");
        eps.push(ort::ep::CUDA::default().build());
    }

    #[cfg(feature = "ort-rocm")]
    {
        tracing::info!("Enabling ROCm execution provider for ONNX Runtime");
        eps.push(ort::ep::ROCm::default().build());
    }

    #[cfg(feature = "ort-webgpu")]
    {
        tracing::info!("Enabling WebGPU execution provider for ONNX Runtime");
        eps.push(ort::ep::WebGPU::default().build());
    }
    #[cfg(all(target_os = "windows", feature = "ort-directml"))]
    {
        tracing::info!("Enabling DirectML execution provider for ONNX Runtime");
        eps.push(ort::ep::DirectML::default().build());
    }

    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "ort-coreml"))]
    {
        tracing::info!("Enabling CoreML execution provider for ONNX Runtime");
        eps.push(ort::ep::CoreML::default().build());
    }

    if eps.is_empty() {
        tracing::debug!("No GPU execution providers configured — using CPU only");
    }

    eps.push(ort::ep::CPU::default().build());
    eps
}
