use crate::core::patterns;
use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn handle(command: &str, output: &str, crp_mode: CrpMode) -> String {
    let original_tokens = count_tokens(output);

    let compressed = match patterns::compress_output(command, output) {
        Some(c) => c,
        None => generic_compress(output),
    };

    if crp_mode.is_tdd() && looks_like_code(&compressed) {
        let ext = detect_ext_from_command(command);
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&compressed, ext);
        for ident in &idents {
            sym.register(ident);
        }
        if !sym.is_empty() {
            let mapped = sym.apply(&compressed);
            let sym_table = sym.format_table();
            let result = format!("{mapped}{sym_table}");
            let sent = count_tokens(&result);
            let savings = protocol::format_savings(original_tokens, sent);
            return format!("{result}\n{savings}");
        }
    }

    let sent = count_tokens(&compressed);
    let savings = protocol::format_savings(original_tokens, sent);

    format!("{compressed}\n{savings}")
}

fn generic_compress(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
        })
        .collect();

    if lines.len() <= 10 {
        return lines.join("\n");
    }

    let first_3 = &lines[..3];
    let last_3 = &lines[lines.len() - 3..];
    format!(
        "{}\n...({} lines omitted)\n{}",
        first_3.join("\n"),
        lines.len() - 6,
        last_3.join("\n")
    )
}

fn looks_like_code(text: &str) -> bool {
    let indicators = [
        "fn ", "pub ", "let ", "const ", "impl ", "struct ", "enum ",
        "function ", "class ", "import ", "export ", "def ", "async ",
        "=>", "->", "::", "self.", "this.",
    ];
    let total_lines = text.lines().count();
    if total_lines < 3 {
        return false;
    }
    let code_lines = text.lines().filter(|l| indicators.iter().any(|i| l.contains(i))).count();
    code_lines as f64 / total_lines as f64 > 0.15
}

fn detect_ext_from_command(command: &str) -> &str {
    let cmd = command.to_lowercase();
    if cmd.contains("cargo") || cmd.contains(".rs") {
        "rs"
    } else if cmd.contains("npm") || cmd.contains("node") || cmd.contains(".ts") || cmd.contains(".js") {
        "ts"
    } else if cmd.contains("python") || cmd.contains("pip") || cmd.contains(".py") {
        "py"
    } else if cmd.contains("go ") || cmd.contains(".go") {
        "go"
    } else {
        "rs"
    }
}
