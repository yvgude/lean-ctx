use super::extract::extract_signatures_ts;
use crate::core::signatures::Signature;

pub(crate) fn extract_sfc_signatures(content: &str) -> Option<Vec<Signature>> {
    let (script_content, line_offset) = extract_script_block_with_offset(content)?;
    let is_ts = content.contains("lang=\"ts\"") || content.contains("lang=\"typescript\"");
    let ext = if is_ts { "ts" } else { "js" };
    let mut sigs = extract_signatures_ts(&script_content, ext)?;
    // Spans are relative to the extracted <script> block; shift them back to
    // file-absolute lines so navigation modes point at the real source line.
    for sig in &mut sigs {
        sig.start_line = sig.start_line.map(|line| line + line_offset);
        sig.end_line = sig.end_line.map(|line| line + line_offset);
    }
    Some(sigs)
}

#[allow(dead_code)]
pub(crate) fn extract_script_block(content: &str) -> Option<String> {
    extract_script_block_with_offset(content).map(|(script, _)| script)
}

fn extract_script_block_with_offset(content: &str) -> Option<(String, usize)> {
    let lower = content.to_lowercase();
    let start_tag_pos = lower.find("<script")?;
    let tag_end = content[start_tag_pos..].find('>')? + start_tag_pos + 1;
    let end_tag = "</script>";
    let end_pos = lower[tag_end..].find(end_tag)? + tag_end;
    let script = &content[tag_end..end_pos];
    if script.trim().is_empty() {
        return None;
    }
    let line_offset = content[..tag_end].bytes().filter(|b| *b == b'\n').count();
    Some((script.to_string(), line_offset))
}
