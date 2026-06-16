//! WASM extension runtime (`wasm-abi-v1`).
//!
//! A sandboxed, language-independent way to contribute extensions (compressors,
//! chunkers, context providers) without recompiling lean-ctx. Guests are plain
//! `.wasm` modules compiled from any language (Rust, AssemblyScript, TinyGo, …)
//! that satisfy a tiny, uniform ABI.
//!
//! ## ABI v1
//!
//! Every guest module exports:
//!
//! - `memory`               — the linear memory (so the host can read/write).
//! - `alloc(n: i32) -> i32` — reserve `n` bytes, return a pointer the host
//!   writes the input into.
//! - one or more **entrypoints** with the uniform signature
//!   `entry(in_ptr: i32, in_len: i32, arg: i32) -> i64`. The return value packs
//!   the output region as `(out_ptr << 32) | out_len` (both `u32`). `arg`
//!   carries an entrypoint-specific integer (compression byte budget, or `0`).
//!
//! Well-known entrypoints:
//!
//! - `lctx_compress` — text → compressed text. `arg` = soft byte budget
//!   (`-1` = none). The host *additionally* enforces the hard budget after
//!   decoding, so a naive guest can never exceed it.
//! - `lctx_provider_fetch` — request JSON → result JSON. `arg` = `0`.
//!
//! ## Why per-call instantiation
//!
//! `wasmi::Store`/`Instance` are not `Sync`, but
//! [`Compressor`](crate::core::extension_registry::Compressor) /
//! [`ContextProvider`](crate::core::providers::ContextProvider) must be. The
//! thread-safe, deterministic pattern is to keep the `Send + Sync`
//! [`wasmi::Engine`] + [`wasmi::Module`] and spin up a fresh `Store` per call.
//! Modules are small and instantiation is cheap; this also gives each call a
//! clean memory (no cross-call state leakage), which is exactly what we want for
//! a deterministic, sandboxed transform.
//!
//! Feature-gated behind `wasm` so the default build stays lean.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use wasmi::{Engine, Linker, Module, Store, TypedFunc};

use crate::core::extension_registry::{Compressor, ExtensionRegistry, truncate_to_budget};
use crate::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

/// Well-known ABI entrypoint names.
pub const ENTRY_COMPRESS: &str = "lctx_compress";
/// Well-known ABI entrypoint for providers.
pub const ENTRY_PROVIDER_FETCH: &str = "lctx_provider_fetch";

/// Error loading or running a WASM extension.
#[derive(Debug, thiserror::Error)]
pub enum WasmError {
    /// Failed to read the `.wasm` file from disk.
    #[error("failed to read wasm at {path}: {source}")]
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The module bytes were invalid or violated the ABI.
    #[error("wasm module error: {0}")]
    Module(String),
    /// A runtime trap or missing export while invoking an entrypoint.
    #[error("wasm runtime error: {0}")]
    Runtime(String),
}

/// A compiled, thread-safe WASM module honoring ABI v1.
///
/// Holds the `Send + Sync` [`Engine`] + [`Module`]; a fresh `Store` is created
/// per invocation (see module docs).
#[derive(Clone)]
pub struct WasmModule {
    engine: Engine,
    module: Module,
}

impl WasmModule {
    /// Compile a module from raw `.wasm` bytes.
    pub fn from_wasm(bytes: &[u8]) -> Result<Self, WasmError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes).map_err(|e| WasmError::Module(e.to_string()))?;
        Ok(Self { engine, module })
    }

    /// Compile a module from a `.wasm` file on disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, WasmError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| WasmError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_wasm(&bytes)
    }

    /// Invoke an ABI v1 entrypoint with `input`, returning the raw output bytes.
    ///
    /// `arg` is forwarded verbatim to the guest (budget for compress, `0` else).
    /// A fresh `Store` + instance is used so calls cannot leak state into one
    /// another.
    pub fn call_bytes(&self, entry: &str, input: &[u8], arg: i32) -> Result<Vec<u8>, WasmError> {
        let mut store = Store::new(&self.engine, ());
        let linker = <Linker<()>>::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| WasmError::Runtime(e.to_string()))?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| WasmError::Runtime("guest does not export `memory`".into()))?;
        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func(&store, "alloc")
            .map_err(|e| WasmError::Runtime(format!("missing `alloc`: {e}")))?;
        let func: TypedFunc<(i32, i32, i32), i64> = instance
            .get_typed_func(&store, entry)
            .map_err(|e| WasmError::Runtime(format!("missing entrypoint `{entry}`: {e}")))?;

        let in_len = i32::try_from(input.len())
            .map_err(|_| WasmError::Runtime("input exceeds 2GiB".into()))?;
        let in_ptr = alloc
            .call(&mut store, in_len)
            .map_err(|e| WasmError::Runtime(format!("alloc trapped: {e}")))?;
        memory
            .write(&mut store, in_ptr as usize, input)
            .map_err(|e| WasmError::Runtime(format!("failed to write input: {e}")))?;

        let packed = func
            .call(&mut store, (in_ptr, in_len, arg))
            .map_err(|e| WasmError::Runtime(format!("entrypoint trapped: {e}")))?;
        let out_ptr = ((packed >> 32) & 0xffff_ffff) as u32 as usize;
        let out_len = (packed & 0xffff_ffff) as u32 as usize;

        let mem = memory.data(&store);
        let end = out_ptr
            .checked_add(out_len)
            .ok_or_else(|| WasmError::Runtime("output region overflows".into()))?;
        if end > mem.len() {
            return Err(WasmError::Runtime(format!(
                "output region [{out_ptr}, {end}) out of bounds (mem {})",
                mem.len()
            )));
        }
        Ok(mem[out_ptr..end].to_vec())
    }
}

// ----------------------------------------------------------------------------
// Compressor (EPIC 12.8)
// ----------------------------------------------------------------------------

/// A [`Compressor`] backed by a WASM `lctx_compress` entrypoint.
///
/// The host enforces the hard byte budget after decoding, so the registered
/// compressor honors the budget contract regardless of guest behavior.
pub struct WasmCompressor {
    name: String,
    module: WasmModule,
}

impl WasmCompressor {
    /// Wrap a compiled module under a registry `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, module: WasmModule) -> Self {
        Self {
            name: name.into(),
            module,
        }
    }

    /// Load a `.wasm` compressor from disk, named `name`.
    pub fn load(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, WasmError> {
        Ok(Self::new(name, WasmModule::from_path(path)?))
    }
}

impl Compressor for WasmCompressor {
    fn name(&self) -> &str {
        &self.name
    }

    fn compress(&self, input: &str, budget: Option<usize>) -> String {
        let arg = budget.and_then(|b| i32::try_from(b).ok()).unwrap_or(-1);
        // Graceful degradation: a faulty guest must never break a read — fall
        // back to the (budgeted) input rather than panicking.
        let out = match self
            .module
            .call_bytes(ENTRY_COMPRESS, input.as_bytes(), arg)
        {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => input.to_string(),
        };
        truncate_to_budget(out, budget)
    }
}

/// Scan `dir` for `*.wasm` files and register each as a compressor named by its
/// file stem. Returns the names registered. Unreadable/invalid modules are
/// skipped (best-effort discovery).
pub fn register_compressors_from_dir(
    reg: &mut ExtensionRegistry,
    dir: impl AsRef<Path>,
) -> Vec<String> {
    let mut registered = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir.as_ref()) else {
        return registered;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(c) = WasmCompressor::load(&path, stem) {
            reg.register_compressor(Arc::new(c));
            registered.push(stem.to_string());
        }
    }
    registered.sort();
    registered
}

// ----------------------------------------------------------------------------
// Provider (EPIC 12.10)
// ----------------------------------------------------------------------------

/// A [`ContextProvider`] backed by a WASM `lctx_provider_fetch` entrypoint.
///
/// The host sends a request JSON (`{"action":…,"params":{…}}`) and parses the
/// guest's result JSON leniently into a [`ProviderResult`], so guests can omit
/// optional fields.
pub struct WasmProvider {
    id: &'static str,
    display: &'static str,
    actions: Vec<&'static str>,
    module: WasmModule,
}

impl WasmProvider {
    /// Build a provider over a compiled module. `id`/`display`/`actions` are
    /// leaked to `'static` once at load (the provider lives for the process).
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        display: impl Into<String>,
        actions: Vec<String>,
        module: WasmModule,
    ) -> Self {
        let id: &'static str = Box::leak(id.into().into_boxed_str());
        let display: &'static str = Box::leak(display.into().into_boxed_str());
        let actions: Vec<&'static str> = actions
            .into_iter()
            .map(|a| &*Box::leak(a.into_boxed_str()))
            .collect();
        Self {
            id,
            display,
            actions,
            module,
        }
    }

    /// Load a `.wasm` provider from disk.
    pub fn load(
        path: impl AsRef<Path>,
        id: impl Into<String>,
        display: impl Into<String>,
        actions: Vec<String>,
    ) -> Result<Self, WasmError> {
        Ok(Self::new(
            id,
            display,
            actions,
            WasmModule::from_path(path)?,
        ))
    }

    fn parse_result(&self, action: &str, bytes: &[u8]) -> Result<ProviderResult, String> {
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("guest returned invalid JSON: {e}"))?;
        let resource_type = v
            .get("resource_type")
            .and_then(Value::as_str)
            .unwrap_or(action)
            .to_string();
        let truncated = v.get("truncated").and_then(Value::as_bool).unwrap_or(false);
        let total_count = v
            .get("total_count")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let items = v
            .get("items")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(item_from_json).collect())
            .unwrap_or_default();
        Ok(ProviderResult {
            provider: self.id.to_string(),
            resource_type,
            items,
            total_count,
            truncated,
        })
    }
}

fn item_from_json(v: &Value) -> ProviderItem {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    ProviderItem {
        id: s("id").unwrap_or_default(),
        title: s("title").unwrap_or_default(),
        state: s("state"),
        author: s("author"),
        created_at: s("created_at"),
        updated_at: s("updated_at"),
        url: s("url"),
        labels: v
            .get("labels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        body: s("body"),
        claims: Vec::new(),
    }
}

/// Sidecar metadata (`<stem>.provider.json`) describing a WASM provider's
/// identity and supported actions, since a bare `.wasm` carries no such info.
#[derive(Debug, serde::Deserialize)]
struct ProviderSidecar {
    id: Option<String>,
    display: Option<String>,
    #[serde(default)]
    actions: Vec<String>,
}

/// Scan `dir` for `*.wasm` providers and register each into `reg`.
///
/// Each module may ship a sidecar `<stem>.provider.json` declaring `id`,
/// `display`, and `actions`. Missing fields default to the file stem / a single
/// `fetch` action. Returns the provider ids registered. Best-effort: unreadable
/// or invalid modules are skipped.
pub fn register_providers_from_dir(
    reg: &crate::core::providers::ProviderRegistry,
    dir: impl AsRef<Path>,
) -> Vec<String> {
    let mut registered = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir.as_ref()) else {
        return registered;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let sidecar: ProviderSidecar =
            std::fs::read_to_string(path.with_extension("provider.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(ProviderSidecar {
                    id: None,
                    display: None,
                    actions: Vec::new(),
                });
        let id = sidecar.id.unwrap_or_else(|| stem.to_string());
        let display = sidecar.display.unwrap_or_else(|| id.clone());
        let actions = if sidecar.actions.is_empty() {
            vec!["fetch".to_string()]
        } else {
            sidecar.actions
        };
        if let Ok(p) = WasmProvider::load(&path, id.clone(), display, actions) {
            reg.register(Arc::new(p));
            registered.push(id);
        }
    }
    registered.sort();
    registered
}

impl ContextProvider for WasmProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display
    }

    fn supported_actions(&self) -> &[&str] {
        &self.actions
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let request = serde_json::json!({
            "action": action,
            "params": {
                "project": params.project,
                "state": params.state,
                "limit": params.limit,
                "query": params.query,
                "id": params.id,
            }
        });
        let req_bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        let out = self
            .module
            .call_bytes(ENTRY_PROVIDER_FETCH, &req_bytes, 0)
            .map_err(|e| e.to_string())?;
        self.parse_result(action, &out)
    }

    fn requires_auth(&self) -> bool {
        false
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ABI-v1 guest, in WAT, exercised through the real wasmi host.
    /// `alloc` is a bump allocator starting past the data segment; `lctx_compress`
    /// echoes its input (host enforces the budget); `lctx_provider_fetch` returns
    /// a fixed 95-byte ProviderResult JSON living at offset 0.
    const GUEST_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $heap (mut i32) (i32.const 1024))
          (data (i32.const 0) "{\"provider\":\"wasm\",\"resource_type\":\"items\",\"items\":[{\"id\":\"1\",\"title\":\"hi\"}],\"truncated\":false}")
          (func (export "alloc") (param $n i32) (result i32)
            (local $p i32)
            (local.set $p (global.get $heap))
            (global.set $heap (i32.add (global.get $heap) (local.get $n)))
            (local.get $p))
          (func (export "lctx_compress") (param $ptr i32) (param $len i32) (param $arg i32) (result i64)
            (i64.or
              (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
              (i64.extend_i32_u (local.get $len))))
          (func (export "lctx_provider_fetch") (param $ptr i32) (param $len i32) (param $arg i32) (result i64)
            (i64.or
              (i64.shl (i64.extend_i32_u (i32.const 0)) (i64.const 32))
              (i64.const 95))))
    "#;

    fn guest() -> WasmModule {
        let bytes = wat::parse_str(GUEST_WAT).expect("valid wat");
        WasmModule::from_wasm(&bytes).expect("module compiles")
    }

    #[test]
    fn host_round_trips_bytes_through_memory() {
        let m = guest();
        let out = m.call_bytes(ENTRY_COMPRESS, b"hello world", -1).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn wasm_compressor_implements_trait_and_honors_budget() {
        let c = WasmCompressor::new("echo", guest());
        assert_eq!(c.name(), "echo");
        // Echo guest returns input; host clamps to the 5-byte budget.
        assert_eq!(c.compress("abcdefgh", Some(5)), "abcde");
        // No budget → full passthrough.
        assert_eq!(c.compress("abc", None), "abc");
    }

    #[test]
    fn wasm_compressor_is_deterministic_and_idempotent() {
        let c = WasmCompressor::new("echo", guest());
        let once = c.compress("repeatable", None);
        let twice = c.compress("repeatable", None);
        assert_eq!(once, twice);
        assert_eq!(c.compress(&once, None), once);
    }

    #[test]
    fn wasm_compressor_registers_as_first_class_extension() {
        let mut reg = ExtensionRegistry::with_builtins();
        reg.register_compressor(Arc::new(WasmCompressor::new("wecho", guest())));
        assert!(reg.compressor_names().contains(&"wecho".to_string()));
        let c = reg.compressor("wecho").unwrap();
        assert_eq!(c.compress("xyz", None), "xyz");
    }

    #[test]
    fn wasm_provider_maps_guest_json_to_result() {
        let p = WasmProvider::new(
            "wasm".to_string(),
            "WASM Provider".to_string(),
            vec!["items".to_string()],
            guest(),
        );
        assert_eq!(p.id(), "wasm");
        assert_eq!(p.supported_actions(), &["items"]);
        assert!(!p.requires_auth());
        assert!(p.is_available());

        let res = p.execute("items", &ProviderParams::default()).unwrap();
        assert_eq!(res.provider, "wasm");
        assert_eq!(res.resource_type, "items");
        assert_eq!(res.items.len(), 1);
        assert_eq!(res.items[0].id, "1");
        assert_eq!(res.items[0].title, "hi");
        assert!(!res.truncated);
    }

    #[test]
    fn missing_entrypoint_is_reported_not_panicked() {
        let m = guest();
        let err = m.call_bytes("nope", b"x", 0).unwrap_err();
        assert!(matches!(err, WasmError::Runtime(_)));
    }

    #[test]
    fn invalid_module_bytes_error() {
        let res = WasmModule::from_wasm(b"not wasm");
        assert!(matches!(res, Err(WasmError::Module(_))));
    }
}
