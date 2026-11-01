// SPDX-License-Identifier: Apache-2.0
//! Configuration set-up shared by tests across the engine and the CLI.

/// Governed mode without a pack: protection is active, so the admitted
/// readers publish their sources into the capture. Call after
/// `isolated_data_dir()`.
pub(crate) fn governed() {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("config");
}
