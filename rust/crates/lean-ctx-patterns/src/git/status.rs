// SPDX-License-Identifier: Apache-2.0

use super::{ahead_re, status_branch_re};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Staged,
    Unstaged,
    Unmerged,
    Untracked,
}

/// `<label>:   <path>` → compact entry. Unknown labels are kept spelled out so
/// no path is ever dropped.
fn entry(label: &str, path: &str) -> String {
    let sigil = match label {
        "new file" => "+",
        "modified" => "~",
        "deleted" => "-",
        "renamed" => "→",
        "copied" => "©",
        "typechange" => "±",
        _ => return format!("{label}:{path}"),
    };
    format!("{sigil}{path}")
}

pub(super) fn compress_status(output: &str) -> String {
    let mut branch = String::new();
    let mut ahead = 0u32;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut unmerged = Vec::new();
    let mut untracked = Vec::new();

    let mut section = Section::None;
    let mut section_has_entries = false;

    for line in output.lines() {
        if let Some(caps) = status_branch_re().captures(line) {
            branch = caps[1].to_string();
        }
        if let Some(caps) = ahead_re().captures(line) {
            ahead = caps[1].parse().unwrap_or(0);
        }

        let trimmed = line.trim();
        let header = match trimmed {
            t if t.starts_with("Changes to be committed") => Some(Section::Staged),
            t if t.starts_with("Changes not staged") => Some(Section::Unstaged),
            t if t.starts_with("Unmerged paths") => Some(Section::Unmerged),
            t if t.starts_with("Untracked files") => Some(Section::Untracked),
            _ => None,
        };
        if let Some(next) = header {
            section = next;
            section_has_entries = false;
            continue;
        }
        // A blank line after the file list closes it; git's trailing hints
        // ("no changes added to commit …") are not paths (#1894).
        if trimmed.is_empty() {
            if section_has_entries {
                section = Section::None;
            }
            continue;
        }
        if section == Section::None || trimmed.starts_with('(') {
            continue;
        }
        section_has_entries = true;

        let item = if section == Section::Untracked {
            trimmed.to_string()
        } else if let Some((label, path)) = trimmed.split_once(':') {
            entry(label.trim(), path.trim())
        } else {
            trimmed.to_string()
        };
        match section {
            Section::Staged => staged.push(item),
            Section::Unstaged => unstaged.push(item),
            Section::Unmerged => unmerged.push(item),
            Section::Untracked => untracked.push(item),
            Section::None => {}
        }
    }

    if branch.is_empty()
        && staged.is_empty()
        && unstaged.is_empty()
        && unmerged.is_empty()
        && untracked.is_empty()
    {
        return output.trim().to_string();
    }

    let mut parts = Vec::new();
    let branch_display = if branch.is_empty() {
        "?".to_string()
    } else {
        branch
    };
    let ahead_str = if ahead > 0 {
        format!(" ↑{ahead}")
    } else {
        String::new()
    };
    parts.push(format!("{branch_display}{ahead_str}"));

    for (name, list) in [
        ("unmerged", &unmerged),
        ("staged", &staged),
        ("unstaged", &unstaged),
        ("untracked", &untracked),
    ] {
        if !list.is_empty() {
            parts.push(format!("{name}: {}", list.join(" ")));
        }
    }

    if output.contains("nothing to commit") && parts.len() == 1 {
        parts.push("clean".to_string());
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::compress_status;

    #[test]
    fn trailing_hint_is_not_an_untracked_path() {
        let out = "On branch main\n\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\tnew_file.rs\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n";
        assert_eq!(compress_status(out), "main\nuntracked: new_file.rs");
    }

    #[test]
    fn every_path_kind_is_kept() {
        let out = "On branch dev\nYour branch is ahead of 'origin/dev' by 2 commits.\n\n\
Unmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   conflict.rs\n\n\
Changes to be committed:\n\tnew file:   a.rs\n\trenamed:    old.rs -> new.rs\n\ttypechange: link\n\n\
Changes not staged for commit:\n\tmodified:   b.rs\n\tdeleted:    gone.rs\n\n\
Untracked files:\n\tu.rs\n";
        assert_eq!(
            compress_status(out),
            "dev ↑2\nunmerged: both modified:conflict.rs\nstaged: +a.rs →old.rs -> new.rs ±link\nunstaged: ~b.rs -gone.rs\nuntracked: u.rs"
        );
    }

    #[test]
    fn clean_tree() {
        let out = "On branch main\nnothing to commit, working tree clean\n";
        assert_eq!(compress_status(out), "main\nclean");
    }
}
