use crate::core::gain::GainEngine;
use crate::core::heatmap;

pub fn handle(action: &str, _path: Option<&str>) -> String {
    let engine = GainEngine::load();

    match action {
        "directory" | "dirs" => heatmap::format_directory_summary(&engine.heatmap),
        "cold" => {
            let all = collect_project_files(_path);
            let cold = engine.heatmap.cold_files(&all, 20);
            if cold.is_empty() {
                "No cold files found (all files have been accessed).".to_string()
            } else {
                let mut lines = vec![format!(
                    "Cold files (never accessed, {} total):",
                    cold.len()
                )];
                for f in &cold {
                    lines.push(format!("  {f}"));
                }
                lines.join("\n")
            }
        }
        "json" => {
            serde_json::to_string_pretty(&engine.heatmap).unwrap_or_else(|_| "{}".to_string())
        }
        _ => heatmap::format_heatmap_status(&engine.heatmap, 20),
    }
}

fn collect_project_files(path: Option<&str>) -> Vec<String> {
    let root = path.unwrap_or(".");
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .max_depth(5)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.')
                && name != "node_modules"
                && name != "target"
                && name != "dist"
                && name != "__pycache__"
                && name != ".git"
        });
    for entry in walker.flatten() {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                if is_source_ext(ext) {
                    files.push(entry.path().to_string_lossy().to_string());
                }
            }
        }
    }
    files
}

fn is_source_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "svelte"
            | "vue"
    )
}
