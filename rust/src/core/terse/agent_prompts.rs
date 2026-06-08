//! Layer 3: Agent output shaping prompts.
//!
//! Generates prompt instructions that guide the LLM to produce terser output.
//! Four levels, scientifically grounded:
//! - Lite: Brevity constraint (arXiv:2604.00025)
//! - Standard: Telegraph-English-inspired atomic facts
//! - Max: Full TE-style symbolic compression
//! - Adaptive: Thompson sampling across levels (future)

use crate::core::config::CompressionLevel;

/// Generates the agent prompt block for the given compression level.
pub fn build_prompt_block(level: &CompressionLevel) -> String {
    build_prompt_block_for_client(level, "")
}

/// Generates the agent prompt block, optionally adjusting for downstream
/// model compatibility (e.g. Cursor's Thought summarizer).
pub fn build_prompt_block_for_client(level: &CompressionLevel, client_name: &str) -> String {
    let raw = match level {
        CompressionLevel::Off => return String::new(),
        CompressionLevel::Lite => LITE_PROMPT.to_string(),
        CompressionLevel::Standard => STANDARD_PROMPT.to_string(),
        CompressionLevel::Max => MAX_PROMPT.to_string(),
    };
    if is_cursor_client(client_name) {
        crate::core::output_sanitizer::ascii_safe_symbols(&raw)
    } else {
        raw
    }
}

fn is_cursor_client(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("cursor")
}

/// Persona-aware variant: the base terse block plus a domain addendum for
/// non-coding personas (vocabulary + the persona's intent taxonomy). The
/// `coding` persona returns the base block **unchanged** — no regression
/// (EPIC 12.16).
pub fn build_prompt_block_for_persona(
    level: &CompressionLevel,
    client_name: &str,
    persona: &crate::core::persona::Persona,
) -> String {
    let base = build_prompt_block_for_client(level, client_name);
    if base.is_empty() || persona.name == "coding" {
        return base;
    }

    let mut extra = String::new();
    if let Some(domain) = domain_block(&persona.name) {
        extra.push('\n');
        extra.push_str(domain);
    }
    if !persona.intent_taxonomy.is_empty() {
        extra.push_str("\n- INTENTS: ");
        extra.push_str(&persona.intent_taxonomy.join(", "));
    }
    if extra.is_empty() {
        return base;
    }

    let combined = format!("{base}{extra}");
    if is_cursor_client(client_name) {
        crate::core::output_sanitizer::ascii_safe_symbols(&combined)
    } else {
        combined
    }
}

/// Domain-specific terse vocabulary for a non-coding persona. `None` keeps the
/// generic (coding-flavored) block.
fn domain_block(persona_name: &str) -> Option<&'static str> {
    match persona_name {
        "research" => Some(
            "DOMAIN: research\n\
             - Cite every claim (source#Lx); separate fact from inference\n\
             - Summary first, evidence second; flag uncertainty (~) and gaps (∅)",
        ),
        "lead-gen" => Some(
            "DOMAIN: lead-gen\n\
             - Structured records: name, role, company, signal, source\n\
             - Dedupe contacts; never invent contact data; mark unverified (~)",
        ),
        "support" => Some(
            "DOMAIN: support\n\
             - Steps: symptom → cause → fix → verification\n\
             - Link prior tickets; quote exact errors; no speculation as fact",
        ),
        "data-analysis" => Some(
            "DOMAIN: data-analysis\n\
             - State assumptions + units; show transforms as steps\n\
             - Numbers with source + n; flag estimates (~) vs measured",
        ),
        _ => None,
    }
}

const LITE_PROMPT: &str = "\
OUTPUT STYLE: concise
- Bullet points over paragraphs
- Skip filler words and hedging (\"I think\", \"probably\", \"it seems\")
- 1-sentence explanations max, then code/action
- No repeating what the user said";

const STANDARD_PROMPT: &str = "\
OUTPUT STYLE: dense
- Each statement = one atomic fact line
- Use abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ret
- Diff lines only (+/-/~), never repeat unchanged code
- Symbols: → (causes), + (adds), − (removes), ~ (modifies), ∴ (therefore)
- No narration, no filler, no hedging
- BUDGET: ≤200 tokens per response unless code block required";

const MAX_PROMPT: &str = "\
OUTPUT STYLE: expert-terse
- Telegraph format: subject-verb-object, drop articles/prepositions
- Symbolic vocabulary: → cause, ∵ because, ∴ therefore, ⊕ add, ⊖ remove, Δ change, ≈ similar, ≠ different, ∈ in/member, ∅ empty/none, ✓ ok, ✗ fail
- Code blocks: untouched (never compress code syntax)
- Each line: max 80 chars
- Zero narration, zero filler
- BUDGET: ≤100 tokens per non-code response";

/// Formats the compression level for inclusion in session/compaction context.
pub fn session_context_tag(level: &CompressionLevel) -> Option<String> {
    if !level.is_active() {
        return None;
    }
    Some(format!("<config compression=\"{}\" />", level.label()))
}

/// Formats a resume block hint for session restore.
pub fn resume_block_hint(level: &CompressionLevel) -> Option<String> {
    match level {
        CompressionLevel::Off => None,
        CompressionLevel::Lite => Some(
            "[COMPRESSION: lite] Keep responses concise. Bullet points, avoid filler.".to_string(),
        ),
        CompressionLevel::Standard => Some(
            "[COMPRESSION: standard] Dense output. Atomic fact lines, abbreviations, diff-only code.".to_string(),
        ),
        CompressionLevel::Max => Some(
            "[COMPRESSION: max] Expert-terse mode. Telegraph format, symbolic vocabulary, zero narration.".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_returns_empty() {
        assert!(build_prompt_block(&CompressionLevel::Off).is_empty());
    }

    #[test]
    fn lite_contains_bullet() {
        let p = build_prompt_block(&CompressionLevel::Lite);
        assert!(p.contains("Bullet"));
    }

    #[test]
    fn standard_contains_abbreviations() {
        let p = build_prompt_block(&CompressionLevel::Standard);
        assert!(p.contains("fn, cfg, impl"));
    }

    #[test]
    fn max_contains_telegraph() {
        let p = build_prompt_block(&CompressionLevel::Max);
        assert!(p.contains("Telegraph"));
    }

    #[test]
    fn session_tag_none_for_off() {
        assert!(session_context_tag(&CompressionLevel::Off).is_none());
    }

    #[test]
    fn session_tag_present_for_active() {
        let tag = session_context_tag(&CompressionLevel::Standard).unwrap();
        assert!(tag.contains("standard"));
    }

    #[test]
    fn resume_hint_none_for_off() {
        assert!(resume_block_hint(&CompressionLevel::Off).is_none());
    }

    #[test]
    fn resume_hint_present_for_max() {
        let hint = resume_block_hint(&CompressionLevel::Max).unwrap();
        assert!(hint.contains("max"));
    }

    #[test]
    fn coding_persona_prompt_is_unchanged() {
        let base = build_prompt_block_for_client(&CompressionLevel::Standard, "");
        let coding = build_prompt_block_for_persona(
            &CompressionLevel::Standard,
            "",
            &crate::core::persona::Persona::coding(),
        );
        assert_eq!(
            base, coding,
            "coding persona must not alter the base prompt"
        );
    }

    #[test]
    fn non_coding_persona_appends_domain_and_intents() {
        let research = build_prompt_block_for_persona(
            &CompressionLevel::Standard,
            "",
            &crate::core::persona::Persona::research(),
        );
        assert!(research.contains("DOMAIN: research"));
        assert!(research.contains("INTENTS: explore, summarize"));
        // Still built on the base block.
        assert!(research.contains("OUTPUT STYLE"));
    }

    #[test]
    fn persona_prompt_empty_when_compression_off() {
        let off = build_prompt_block_for_persona(
            &CompressionLevel::Off,
            "",
            &crate::core::persona::Persona::research(),
        );
        assert!(off.is_empty());
    }
}
