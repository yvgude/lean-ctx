// SPDX-License-Identifier: Apache-2.0
//! Walk limits and file filters shared by `ctx_search` and the search index,
//! so both see the same searchable universe. Moved down from
//! `tools/ctx_search/implementation.rs`.

use std::path::Path;

/// Size cap for a file met during a directory walk.
pub const MAX_FILE_SIZE: u64 = 512_000;
/// Maximum directory depth of a search walk.
pub const MAX_WALK_DEPTH: usize = 20;

pub fn is_binary_ext(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "svg"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "pdf"
            | "zip"
            | "tar"
            | "gz"
            | "br"
            | "zst"
            | "bz2"
            | "xz"
            | "mp3"
            | "mp4"
            | "webm"
            | "ogg"
            | "wasm"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "lock"
            | "map"
            | "snap"
            | "patch"
            | "db"
            | "sqlite"
            | "parquet"
            | "arrow"
            | "bin"
            | "o"
            | "a"
            | "class"
            | "pyc"
            | "pyo"
    )
}

pub fn is_generated_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".bundle.js")
        || name.ends_with(".chunk.js")
        || name.ends_with(".d.ts")
        || name.ends_with(".js.map")
        || name.ends_with(".css.map")
}
