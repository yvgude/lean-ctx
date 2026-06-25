use crate::core::profiles::LayoutConfig;

#[derive(Debug, Clone)]
pub struct LayoutApplyResultV1 {
    pub output: String,
    pub changed: bool,
    pub skipped: bool,
    pub reason_code: String,
}

#[must_use]
pub fn maybe_reorder_for_attention(
    content: &str,
    task_keywords: &[String],
    cfg: &LayoutConfig,
) -> LayoutApplyResultV1 {
    if !cfg.enabled_effective() {
        return LayoutApplyResultV1 {
            output: content.to_string(),
            changed: false,
            skipped: true,
            reason_code: "disabled".to_string(),
        };
    }

    let line_count = content.lines().count();
    if line_count < cfg.min_lines_effective() {
        return LayoutApplyResultV1 {
            output: content.to_string(),
            changed: false,
            skipped: true,
            reason_code: "below_min_lines".to_string(),
        };
    }

    if task_keywords.is_empty() {
        return LayoutApplyResultV1 {
            output: content.to_string(),
            changed: false,
            skipped: true,
            reason_code: "no_keywords".to_string(),
        };
    }

    let output = crate::core::neural::context_reorder::reorder_for_lcurve(content, task_keywords);
    LayoutApplyResultV1 {
        changed: output != content,
        output,
        skipped: false,
        reason_code: "reordered".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_when_disabled() {
        let cfg = LayoutConfig::default();
        let content = "use std::io;\nfn main() {}\nlet x = 1;\nlet y = 2;\nlet z = 3;\n";
        let r = maybe_reorder_for_attention(content, &["main".to_string()], &cfg);
        assert!(r.skipped);
        assert!(!r.changed);
        assert_eq!(r.output, content);
    }

    #[test]
    fn skips_when_no_keywords() {
        let cfg = LayoutConfig {
            enabled: Some(true),
            min_lines: Some(1),
        };
        let content = "use std::io;\nfn main() {}\n";
        let r = maybe_reorder_for_attention(content, &[], &cfg);
        assert!(r.skipped);
        assert!(!r.changed);
        assert_eq!(r.output, content);
    }

    #[test]
    fn reorders_when_enabled_and_keywords_present() {
        let cfg = LayoutConfig {
            enabled: Some(true),
            min_lines: Some(1),
        };
        let content = "let x = 1;\nuse std::io;\n}\nreturn Err(e);\npub struct Foo {\nfn main() {";
        let r = maybe_reorder_for_attention(content, &["err".to_string()], &cfg);
        assert!(!r.skipped);
        assert!(r.output.starts_with("return Err(") || r.output.starts_with("use "));
    }
}
