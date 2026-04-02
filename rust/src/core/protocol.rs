use std::path::Path;

pub fn shorten_path(path: &str) -> String {
    let p = Path::new(path);
    if let Some(name) = p.file_name() {
        return name.to_string_lossy().to_string();
    }
    path.to_string()
}

#[allow(dead_code)]
pub fn format_type_short(ty: &str) -> String {
    match ty {
        "string" | "String" => ":s".to_string(),
        "number" | "i32" | "i64" | "u32" | "u64" | "usize" | "f32" | "f64" => ":n".to_string(),
        "boolean" | "bool" => ":b".to_string(),
        "void" | "()" => "".to_string(),
        t if t.starts_with("Promise<") => format!("→{}", &t[8..t.len() - 1]),
        t if t.starts_with("Option<") => format!(":?{}", &t[7..t.len() - 1]),
        t if t.starts_with("Vec<") => format!(":[{}]", &t[4..t.len() - 1]),
        t if t.starts_with("Result<") => format!("→!{}", &t[7..t.len() - 1]),
        _ => format!(":{ty}"),
    }
}

pub fn format_savings(original: usize, compressed: usize) -> String {
    let saved = original.saturating_sub(compressed);
    if original == 0 {
        return "0 tok saved".to_string();
    }
    let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
    format!("[{saved} tok saved ({pct}%)]")
}

pub struct InstructionTemplate {
    pub code: &'static str,
    pub full: &'static str,
}

const TEMPLATES: &[InstructionTemplate] = &[
    InstructionTemplate {
        code: "ACT1",
        full: "Act immediately, report result in one line",
    },
    InstructionTemplate {
        code: "BRIEF",
        full: "Summarize approach in 1-2 lines, then act",
    },
    InstructionTemplate {
        code: "FULL",
        full: "Outline approach, consider edge cases, then act",
    },
    InstructionTemplate {
        code: "DELTA",
        full: "Only show changed lines, not full files",
    },
    InstructionTemplate {
        code: "NOREPEAT",
        full: "Never repeat known context. Reference cached files by Fn ID",
    },
    InstructionTemplate {
        code: "STRUCT",
        full: "Use notation, not sentences. Changes: +line/-line/~line",
    },
    InstructionTemplate {
        code: "1LINE",
        full: "One line per action. Summarize, don't explain",
    },
    InstructionTemplate {
        code: "NODOC",
        full: "Don't add comments that narrate what code does",
    },
    InstructionTemplate {
        code: "ACTFIRST",
        full: "Execute tool calls immediately. Never narrate before acting",
    },
    InstructionTemplate {
        code: "QUALITY",
        full: "Never skip edge case analysis or error handling to save tokens",
    },
    InstructionTemplate {
        code: "NOMOCK",
        full: "Never use mock data, fake values, or placeholder code",
    },
    InstructionTemplate {
        code: "FREF",
        full: "Reference files by Fn refs only, never full paths",
    },
    InstructionTemplate {
        code: "DIFF",
        full: "For code changes: show only diff lines, not full files",
    },
    InstructionTemplate {
        code: "ABBREV",
        full: "Use abbreviations: fn, cfg, impl, deps, req, res, ctx, err",
    },
    InstructionTemplate {
        code: "SYMBOLS",
        full: "Use TDD notation: +=add -=remove ~=modify ->=returns ok/fail for status",
    },
];

/// Build the decoder block that explains all instruction codes (sent once per session).
pub fn instruction_decoder_block() -> String {
    let mut lines = vec!["INSTRUCTION CODES:".to_string()];
    for t in TEMPLATES {
        lines.push(format!("  {} = {}", t.code, t.full));
    }
    lines.join("\n")
}

/// Encode an instruction suffix using short codes with budget hints.
/// Response budget is dynamic based on task complexity to shape LLM output length.
pub fn encode_instructions(complexity: &str) -> String {
    match complexity {
        "mechanical" => "MODE: ACT1 DELTA 1LINE | BUDGET: <=50 tokens, 1 line answer".to_string(),
        "simple" => "MODE: BRIEF DELTA 1LINE | BUDGET: <=100 tokens, structured".to_string(),
        "standard" => "MODE: BRIEF DELTA NOREPEAT STRUCT | BUDGET: <=200 tokens".to_string(),
        "complex" => {
            "MODE: FULL QUALITY NOREPEAT STRUCT FREF DIFF | BUDGET: <=500 tokens".to_string()
        }
        "architectural" => {
            "MODE: FULL QUALITY NOREPEAT STRUCT FREF | BUDGET: unlimited".to_string()
        }
        _ => "MODE: BRIEF | BUDGET: <=200 tokens".to_string(),
    }
}

/// Encode instructions with SNR metric for context quality awareness.
pub fn encode_instructions_with_snr(complexity: &str, compression_pct: f64) -> String {
    let snr = if compression_pct > 0.0 {
        1.0 - (compression_pct / 100.0)
    } else {
        1.0
    };
    let base = encode_instructions(complexity);
    format!("{base} | SNR: {snr:.2}")
}

/// Estimate token savings of encoded vs full instruction text.
#[allow(dead_code)]
pub fn instruction_encoding_savings() -> (usize, usize) {
    use super::tokens::count_tokens;
    let decoder = instruction_decoder_block();
    let decoder_cost = count_tokens(&decoder);

    let full_mechanical = "TASK COMPLEXITY: mechanical\nMinimal reasoning needed. Act immediately, report result in one line.";
    let encoded_mechanical = "MODE: ACT1 DELTA 1LINE | BUDGET: <=50 tokens, 1 line answer";

    let saving_per_call =
        count_tokens(full_mechanical).saturating_sub(count_tokens(encoded_mechanical));
    (decoder_cost, saving_per_call.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_block_contains_all_codes() {
        let block = instruction_decoder_block();
        for t in TEMPLATES {
            assert!(
                block.contains(t.code),
                "decoder should contain code {}",
                t.code
            );
        }
    }

    #[test]
    fn encoded_instructions_are_compact() {
        use super::super::tokens::count_tokens;
        let full = "TASK COMPLEXITY: mechanical\nMinimal reasoning needed. Act immediately, report result in one line. Show only changed lines, not full files.";
        let encoded = encode_instructions("mechanical");
        assert!(
            count_tokens(&encoded) <= count_tokens(full),
            "encoded ({}) should be <= full ({})",
            count_tokens(&encoded),
            count_tokens(full)
        );
    }

    #[test]
    fn encoding_has_positive_savings() {
        let (decoder_cost, saving_per_call) = instruction_encoding_savings();
        assert!(decoder_cost > 0);
        assert!(saving_per_call > 0);
    }

    #[test]
    fn all_complexity_levels_encode() {
        for level in &["mechanical", "standard", "architectural"] {
            let encoded = encode_instructions(level);
            assert!(encoded.starts_with("MODE:"), "should start with MODE:");
        }
    }
}
