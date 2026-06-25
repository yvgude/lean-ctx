//! Godot-specific parsers and helpers.
//!
//! `GDScript` (`.gd`) source is handled by the generic tree-sitter pipeline; this
//! module covers the Godot-native *resource* formats that are not source code
//! but still carry navigable graph dependencies — most importantly the
//! `PackedScene` text format (`.tscn`) which links scenes to their scripts. (#316)

pub mod scene;
