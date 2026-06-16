//! Write primitives: marker-section replace, shared append, dedicated write.
//! All writes go through `config_io::write_atomic_with_backup`.

use super::content::{RULES_SHARED, rules_content};
use super::{END_MARKER, MARKER, RULES_VERSION, RulesFormat, RulesResult, RulesTarget};

pub(super) fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        if content.contains(MARKER) {
            if content.contains(RULES_VERSION) {
                return Ok(RulesResult::AlreadyPresent);
            }
            ensure_parent(&target.path)?;
            return match target.format {
                RulesFormat::SharedMarkdown => replace_markdown_section(&target.path, &content),
                RulesFormat::DedicatedMarkdown | RulesFormat::CursorMdc => {
                    write_dedicated(&target.path, rules_content(&target.format))
                }
            };
        }
    }

    ensure_parent(&target.path)?;

    match target.format {
        RulesFormat::SharedMarkdown => append_to_shared(&target.path),
        RulesFormat::DedicatedMarkdown | RulesFormat::CursorMdc => {
            write_dedicated(&target.path, rules_content(&target.format))
        }
    }
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn append_to_shared(path: &std::path::Path) -> Result<RulesResult, String> {
    let mut content = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(RULES_SHARED);
    content.push('\n');

    crate::config_io::write_atomic_with_backup(path, &content)?;
    Ok(RulesResult::Injected)
}

pub(super) fn replace_markdown_section(
    path: &std::path::Path,
    content: &str,
) -> Result<RulesResult, String> {
    let start = content.find(MARKER);
    let end = content.find(END_MARKER);

    let new_content = match (start, end) {
        (Some(s), Some(e)) => {
            let before = &content[..s];
            let after_end = e + END_MARKER.len();
            let after = content[after_end..].trim_start_matches('\n');
            let mut result = before.to_string();
            result.push_str(RULES_SHARED);
            if !after.is_empty() {
                result.push('\n');
                result.push_str(after);
            }
            result
        }
        (Some(s), None) => {
            let before = &content[..s];
            let mut result = before.to_string();
            result.push_str(RULES_SHARED);
            result.push('\n');
            result
        }
        _ => return Ok(RulesResult::AlreadyPresent),
    };

    crate::config_io::write_atomic_with_backup(path, &new_content)?;
    Ok(RulesResult::Updated)
}

pub(super) fn write_dedicated(
    path: &std::path::Path,
    content: &'static str,
) -> Result<RulesResult, String> {
    if !path.exists() {
        crate::config_io::write_atomic_with_backup(path, content)?;
        return Ok(RulesResult::Injected);
    }

    let existing = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if !existing.contains(MARKER) {
        crate::config_io::write_atomic_with_backup(path, content)?;
        return Ok(RulesResult::Injected);
    }

    let start = existing.find(MARKER);
    let end = existing.find(END_MARKER);

    let (before, after) = match (start, end) {
        (Some(s), Some(e)) => {
            let before = &existing[..s];
            let after_end = e + END_MARKER.len();
            let after = existing[after_end..].trim_start_matches('\n');
            (before.to_string(), after.to_string())
        }
        (Some(s), None) => (existing[..s].to_string(), String::new()),
        _ => (String::new(), String::new()),
    };

    let has_user_content = !before.trim().is_empty() || !after.trim().is_empty();

    if has_user_content {
        let new_section = if let Some(marker_pos) = content.find(MARKER) {
            &content[marker_pos..]
        } else {
            content
        };

        let mut result = before.clone();
        result.push_str(new_section);
        if !after.is_empty() {
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&after);
        }
        if !result.ends_with('\n') {
            result.push('\n');
        }
        crate::config_io::write_atomic_with_backup(path, &result)?;
    } else {
        crate::config_io::write_atomic_with_backup(path, content)?;
    }

    Ok(RulesResult::Updated)
}
