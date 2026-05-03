const MAX_METADATA_LEN: usize = 200;

pub fn neutralize_metadata(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(MAX_METADATA_LEN));
    let mut count = 0usize;
    for ch in input.chars() {
        if count >= MAX_METADATA_LEN {
            out.push('…');
            break;
        }
        if (ch as u32) < 0x20 && ch != '\n' && ch != '\t' && ch != '\r' {
            continue;
        }
        match ch {
            '<' => out.push('‹'),
            '>' => out.push('›'),
            '`' => out.push('\''),
            _ => out.push(ch),
        }
        count += 1;
    }
    out
}

pub fn neutralize_shell_content(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let chars: Vec<char> = input.chars().collect();
    while i < chars.len() {
        let ch = chars[i];
        if (ch as u32) < 0x20 && ch != '\n' && ch != '\t' && ch != '\r' {
            i += 1;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn safe_label(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if ch == '_' || ch == '-' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "BLOCK".to_string()
    } else {
        out
    }
}

pub fn fence_content(label: &str, content: &str) -> String {
    let label = safe_label(label);
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    let token = to_hex(&bytes);
    let marker = format!("LCTX_{label}_{token}");
    format!("‹‹‹{marker}›››\n{content}\n‹‹‹{marker}›››")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutralize_replaces_angle_and_backticks() {
        let s = "<tag>`code`</tag>";
        let out = neutralize_metadata(s);
        assert!(out.contains('‹'));
        assert!(out.contains('›'));
        assert!(!out.contains('`'));
    }

    #[test]
    fn fence_wraps_symmetrically() {
        let out = fence_content("knowledge", "hello");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 3);
        assert_eq!(lines[0], lines[lines.len() - 1]);
    }
}
