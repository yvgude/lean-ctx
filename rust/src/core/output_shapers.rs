// SPDX-License-Identifier: Apache-2.0
//! The shapers behind `ctx_shape`. The shaping pipeline belongs to the proxy
//! layer, which registers it at start-up (`proxy::register_shapers`, called
//! by the CLI); the engine's `ctx_shape` tool runs whatever is registered.
//! Without a registration (library use, tests) there is nothing to shape
//! with, so `ctx_shape` hands the redacted output back unshaped.

use std::sync::OnceLock;

/// Shapes a native shell command's output: `(command, output, exit_code)`.
pub type CommandShaper = fn(&str, &str, Option<i32>) -> String;
/// Shapes any other native tool's output: `(output, tool_name)`.
pub type ToolShaper = fn(&str, Option<&str>) -> String;

/// The registered shaping entry points.
#[derive(Clone, Copy)]
pub struct OutputShapers {
    pub command: CommandShaper,
    pub tool: ToolShaper,
}

static SHAPERS: OnceLock<OutputShapers> = OnceLock::new();

/// Register the output shapers. The first registration wins.
pub fn register_output_shapers(shapers: OutputShapers) {
    let _ = SHAPERS.set(shapers);
}

/// The registered output shapers, if any.
pub fn output_shapers() -> Option<&'static OutputShapers> {
    SHAPERS.get()
}
