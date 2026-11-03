// SPDX-License-Identifier: Apache-2.0
//! Tests for `ctx_read`. Extracted from `ctx_read/mod.rs`;
//! `super::*` resolves to the `ctx_read` module.

use super::*;

#[cfg(test)]
#[path = "tests_compression.rs"]
mod tests_compression;
#[cfg(test)]
#[path = "tests_e26_benchmark.rs"]
mod tests_e26_benchmark;
#[cfg(test)]
#[path = "tests_inflation.rs"]
mod tests_inflation;
#[cfg(test)]
#[path = "tests_modes.rs"]
mod tests_modes;
#[cfg(test)]
#[path = "tests_ranges.rs"]
mod tests_ranges;
#[cfg(test)]
#[path = "tests_verbatim.rs"]
mod tests_verbatim;
