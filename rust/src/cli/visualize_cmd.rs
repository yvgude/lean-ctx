//! CLI handler for `lean-ctx visualize`.
//!
//! Collects graph, knowledge, heatmap, and session data from the current
//! project, renders a self-contained HTML report, and optionally opens it
//! in the default browser.

use crate::core::visualizer;

pub(crate) fn cmd_visualize(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let output = extract_output_path(args);
    let should_open = args.iter().any(|a| a == "--open");

    eprintln!("Collecting data from {project_root}...");
    let data = visualizer::collect_data(&project_root);

    let node_count = data.graph.nodes.len();
    let edge_count = data.graph.edges.len();
    let fact_count = data.knowledge.len();
    let file_count = data.savings.files.len();

    let html = visualizer::render_html(&data);

    if let Err(e) = std::fs::write(&output, &html) {
        eprintln!("Error writing {output}: {e}");
        std::process::exit(1);
    }

    eprintln!("Wrote {output} ({:.1} KB)", html.len() as f64 / 1024.0);
    eprintln!("  Graph: {node_count} nodes, {edge_count} edges");
    eprintln!("  Knowledge: {fact_count} facts");
    eprintln!(
        "  Savings: {file_count} files tracked, {saved} tokens saved",
        saved = data.savings.total_saved
    );

    if should_open {
        let abs = std::path::Path::new(&output)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&output));
        let url = format!("file://{}", abs.display());
        if open_browser(&url).is_err() {
            eprintln!("Could not open browser. Open manually: {url}");
        }
    }
}

fn extract_output_path(args: &[String]) -> String {
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--output=")
            && !v.trim().is_empty()
        {
            return v.to_string();
        }
        if (a == "--output" || a == "-o")
            && let Some(v) = it.peek()
            && !v.starts_with("--")
        {
            return (*v).clone();
        }
    }
    "lean-ctx-report.html".to_string()
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_output_default() {
        let args: Vec<String> = vec![];
        assert_eq!(extract_output_path(&args), "lean-ctx-report.html");
    }

    #[test]
    fn extract_output_equals_syntax() {
        let args: Vec<String> = vec!["--output=report.html".to_string()];
        assert_eq!(extract_output_path(&args), "report.html");
    }

    #[test]
    fn extract_output_separate_arg() {
        let args: Vec<String> = vec!["--output".to_string(), "out.html".to_string()];
        assert_eq!(extract_output_path(&args), "out.html");
    }

    #[test]
    fn extract_output_short_flag() {
        let args: Vec<String> = vec!["-o".to_string(), "my-report.html".to_string()];
        assert_eq!(extract_output_path(&args), "my-report.html");
    }
}
