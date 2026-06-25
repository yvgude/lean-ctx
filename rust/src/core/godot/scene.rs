//! Minimal parser for Godot `PackedScene` text files (`.tscn`).
//!
//! `.tscn` is an INI-like format whose `[ext_resource …]` headers declare the
//! external resources a scene depends on — crucially the scripts attached to
//! its nodes. We surface those script/scene dependencies as [`ImportInfo`] so
//! the graph index can materialise Scene→Script (and Scene→Scene) edges through
//! the shared `GDScript` resolver (`res://` paths resolve identically to a
//! `preload`). Binary assets (textures, audio, fonts) are intentionally skipped:
//! they are not navigable source nodes. (#316)

use crate::core::deep_queries::{ImportInfo, ImportKind};

/// Resource extensions that name a navigable graph node (script or sub-scene).
const SCENE_SOURCE_EXTS: [&str; 2] = ["gd", "tscn"];

/// Extracts the script/scene dependencies declared by a `.tscn` file's
/// `[ext_resource]` headers, as resolver-ready [`ImportInfo`] entries.
#[must_use]
pub fn extract_scene_imports(content: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("[ext_resource") {
            continue;
        }
        let Some(path) = extract_attr(trimmed, "path") else {
            continue;
        };
        if !is_scene_source(&path) {
            continue;
        }
        imports.push(ImportInfo {
            source: path,
            names: Vec::new(),
            kind: ImportKind::SideEffect,
            line: idx + 1,
            is_type_only: false,
        });
    }
    imports
}

/// Extracts the value of a space-separated `key="value"` attribute from a
/// `.tscn` header line. The key must sit on a token boundary so `path=` is not
/// matched inside a hypothetical `subpath=`.
fn extract_attr(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let bytes = line.as_bytes();
    let value_start = line.match_indices(&needle).find_map(|(i, _)| {
        let boundary = i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'[';
        boundary.then_some(i + needle.len())
    })?;
    let rest = &line[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn is_scene_source(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SCENE_SOURCE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENE: &str = r#"[gd_scene load_steps=4 format=3 uid="uid://abc123"]

[ext_resource type="Script" path="res://actors/Player.gd" id="1_script"]
[ext_resource type="PackedScene" uid="uid://def456" path="res://scenes/Enemy.tscn" id="2_scene"]
[ext_resource type="Texture2D" path="res://art/player.png" id="3_tex"]

[node name="Player" type="CharacterBody2D"]
script = ExtResource("1_script")

[node name="Enemy" parent="." instance=ExtResource("2_scene")]
"#;

    #[test]
    fn extracts_script_and_scene_paths() {
        let imports = extract_scene_imports(SCENE);
        let sources: Vec<&str> = imports.iter().map(|i| i.source.as_str()).collect();
        assert!(
            sources.contains(&"res://actors/Player.gd"),
            "got {sources:?}"
        );
        assert!(
            sources.contains(&"res://scenes/Enemy.tscn"),
            "got {sources:?}"
        );
    }

    #[test]
    fn skips_binary_assets() {
        let imports = extract_scene_imports(SCENE);
        let sources: Vec<&str> = imports.iter().map(|i| i.source.as_str()).collect();
        assert!(
            !sources.contains(&"res://art/player.png"),
            "binary assets must not become graph edges; got {sources:?}"
        );
    }

    #[test]
    fn records_line_numbers_and_side_effect_kind() {
        let imports = extract_scene_imports(SCENE);
        let script = imports
            .iter()
            .find(|i| i.source == "res://actors/Player.gd")
            .unwrap();
        assert_eq!(script.line, 3, "1-based line of the ext_resource header");
        assert_eq!(script.kind, ImportKind::SideEffect);
    }

    #[test]
    fn ignores_files_without_ext_resources() {
        let imports = extract_scene_imports("[gd_scene format=3]\n[node name=\"Root\"]\n");
        assert!(imports.is_empty());
    }
}
