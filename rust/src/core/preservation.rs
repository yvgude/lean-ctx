use crate::core::deps;
use crate::core::signatures;

#[derive(Debug, Clone, Default)]
pub struct PreservationScore {
    pub functions_total: usize,
    pub functions_preserved: usize,
    pub exports_total: usize,
    pub exports_preserved: usize,
    pub imports_total: usize,
    pub imports_preserved: usize,
}

impl PreservationScore {
    pub fn function_rate(&self) -> f64 {
        if self.functions_total == 0 {
            return 1.0;
        }
        self.functions_preserved as f64 / self.functions_total as f64
    }

    pub fn export_rate(&self) -> f64 {
        if self.exports_total == 0 {
            return 1.0;
        }
        self.exports_preserved as f64 / self.exports_total as f64
    }

    pub fn import_rate(&self) -> f64 {
        if self.imports_total == 0 {
            return 1.0;
        }
        self.imports_preserved as f64 / self.imports_total as f64
    }

    pub fn overall(&self) -> f64 {
        let total = self.functions_total + self.exports_total + self.imports_total;
        if total == 0 {
            return 1.0;
        }
        let preserved = self.functions_preserved + self.exports_preserved + self.imports_preserved;
        preserved as f64 / total as f64
    }
}

pub fn measure(raw_content: &str, compressed_output: &str, ext: &str) -> PreservationScore {
    let sigs = signatures::extract_signatures(raw_content, ext);
    let dep_info = deps::extract_deps(raw_content, ext);

    let function_names: Vec<&str> = sigs
        .iter()
        .filter(|s| matches!(s.kind, "fn" | "method"))
        .map(|s| s.name.as_str())
        .collect();

    let class_names: Vec<&str> = sigs
        .iter()
        .filter(|s| matches!(s.kind, "class" | "struct" | "interface" | "trait" | "enum"))
        .map(|s| s.name.as_str())
        .collect();

    let all_symbols: Vec<&str> = function_names
        .iter()
        .chain(class_names.iter())
        .copied()
        .collect();

    let functions_preserved = all_symbols
        .iter()
        .filter(|name| !name.is_empty() && compressed_output.contains(*name))
        .count();

    let exports_preserved = dep_info
        .exports
        .iter()
        .filter(|e| !e.is_empty() && compressed_output.contains(e.as_str()))
        .count();

    let imports_preserved = dep_info
        .imports
        .iter()
        .filter(|i| !i.is_empty() && compressed_output.contains(i.as_str()))
        .count();

    PreservationScore {
        functions_total: all_symbols.len(),
        functions_preserved,
        exports_total: dep_info.exports.len(),
        exports_preserved,
        imports_total: dep_info.imports.len(),
        imports_preserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_reports_full_preservation_when_all_symbols_present() {
        let raw = r"
use crate::core::deps;

pub fn build_graph() -> usize { 1 }
pub struct GraphNode;
";
        let compressed = "build_graph GraphNode crate::core::deps";

        let score = measure(raw, compressed, "rs");
        assert_eq!(score.functions_total, 2);
        assert_eq!(score.functions_preserved, 2);
        assert_eq!(score.exports_total, 2);
        assert_eq!(score.exports_preserved, 2);
        assert_eq!(score.imports_total, 1);
        assert_eq!(score.imports_preserved, 1);
        assert!((score.overall() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn measure_detects_missing_symbols() {
        let raw = r"
use crate::core::deps;
pub fn build_graph() -> usize { 1 }
pub struct GraphNode;
";
        let compressed = "build_graph";

        let score = measure(raw, compressed, "rs");
        assert_eq!(score.functions_total, 2);
        assert_eq!(score.functions_preserved, 1);
        assert_eq!(score.exports_total, 2);
        assert_eq!(score.exports_preserved, 1);
        assert_eq!(score.imports_total, 1);
        assert_eq!(score.imports_preserved, 0);
        assert!(score.overall() < 1.0);
    }

    #[test]
    fn measure_defaults_to_full_when_no_trackable_entities() {
        let raw = "plain text without code signatures";
        let compressed = "short summary";
        let score = measure(raw, compressed, "txt");

        assert_eq!(score.functions_total, 0);
        assert_eq!(score.exports_total, 0);
        assert_eq!(score.imports_total, 0);
        assert!((score.function_rate() - 1.0).abs() < f64::EPSILON);
        assert!((score.export_rate() - 1.0).abs() < f64::EPSILON);
        assert!((score.import_rate() - 1.0).abs() < f64::EPSILON);
        assert!((score.overall() - 1.0).abs() < f64::EPSILON);
    }
}
