//! Cognitive Load Theory–style scoring for code context (intrinsic / extraneous / germane).

/// Decomposed load estimates in arbitrary comparable units (then normalized).
#[derive(Debug, Clone, PartialEq)]
pub struct CognitiveLoadScore {
    pub intrinsic: f64,
    pub extraneous: f64,
    pub germane: f64,
    pub total: f64,
    pub recommendation: String,
}

fn max_brace_depth(s: &str) -> usize {
    let mut d = 0usize;
    let mut maxd = 0usize;
    let mut line_comment = false;
    let mut block = 0usize;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if line_comment {
            if c == '\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block > 0 {
            if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                block += 1;
                i += 2;
                continue;
            }
            if c == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                block = block.saturating_sub(1);
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < chars.len() {
            if chars[i + 1] == '/' {
                line_comment = true;
                i += 2;
                continue;
            }
            if chars[i + 1] == '*' {
                block += 1;
                i += 2;
                continue;
            }
        }
        match c {
            '{' | '(' | '[' => {
                d += 1;
                maxd = maxd.max(d);
            }
            '}' | ')' | ']' => {
                d = d.saturating_sub(1);
            }
            _ => {}
        }
        i += 1;
    }
    maxd
}

fn intrinsic_raw(content: &str) -> f64 {
    let depth = max_brace_depth(content) as f64;
    let lets = content.matches("let ").count() as f64;
    let ctrl = ["for ", "while ", "match ", "if ", "else", "loop {"]
        .iter()
        .map(|k| content.matches(*k).count() as f64)
        .sum::<f64>();
    depth * 0.22 + lets * 0.06 + ctrl * 0.05
}

fn extraneous_raw(content: &str) -> f64 {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len().max(1) as f64;
    let blank = lines.iter().filter(|l| l.trim().is_empty()).count() as f64 / n;
    let mut comment_lines = 0usize;
    let mut block = false;
    for line in &lines {
        let t = line.trim_start();
        if block {
            comment_lines += 1;
            if t.contains("*/") {
                block = false;
            }
            continue;
        }
        if t.starts_with("//") {
            comment_lines += 1;
        } else if t.starts_with("/*") {
            comment_lines += 1;
            block = !t.contains("*/");
        }
    }
    let comment_ratio = comment_lines as f64 / n;
    let boiler = [
        "todo!",
        "unwrap()",
        "expect(",
        "derive(Default)",
        "println!",
        "#[",
    ]
    .iter()
    .map(|p| content.matches(*p).count() as f64)
    .sum::<f64>();
    comment_ratio * 1.1 + blank * 0.9 + boiler * 0.08
}

fn camel_type_tokens(content: &str) -> usize {
    content
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|tok| tok.len() >= 2 && tok.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .count()
}

fn germane_raw(content: &str) -> f64 {
    let n_types = camel_type_tokens(content);
    let apiish = content.matches('.').count() as f64 * 0.02;
    let algo = [
        "sort",
        "binary",
        "hash",
        "graph",
        "dfs",
        "bfs",
        "heap",
        "recursive",
    ]
    .iter()
    .map(|k| content.to_ascii_lowercase().matches(k).count() as f64)
    .sum::<f64>();
    n_types as f64 * 0.09 + apiish + algo * 0.11
}

fn norm(x: f64) -> f64 {
    (x / (x + 1.2)).min(1.0)
}

fn recommend(intr: f64, extr: f64, germ: f64) -> String {
    if extr >= intr * 1.35 && extr >= 0.28 {
        return "entropy or aggressive — dominant extraneous noise".to_string();
    }
    if intr >= germ * 1.2 && intr >= 0.32 {
        return "signatures or map — high intrinsic complexity".to_string();
    }
    if germ >= intr * 1.05 && germ >= 0.28 {
        return "full or reference — strong germane / API signal".to_string();
    }
    if extr >= 0.22 && extr >= intr {
        return "aggressive — moderate clutter".to_string();
    }
    "auto — balanced load profile".to_string()
}

/// Score cognitive-load dimensions and suggest a compression mode family.
#[must_use]
pub fn score_cognitive_load(content: &str) -> CognitiveLoadScore {
    let i = norm(intrinsic_raw(content));
    let e = norm(extraneous_raw(content));
    let g = norm(germane_raw(content));
    let total = i + e + g;
    let recommendation = recommend(i, e, g);
    CognitiveLoadScore {
        intrinsic: i,
        extraneous: e,
        germane: g,
        total,
        recommendation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_script_low_scores() {
        let s = score_cognitive_load("fn main() {}\n");
        assert!(s.total < 2.5);
        assert!(s.intrinsic > 0.0);
    }

    #[test]
    fn nested_logic_raises_intrinsic() {
        let code = r"
fn x() {
    if true {
        for _ in 0..10 {
            while false {}
        }
    }
}
";
        let s = score_cognitive_load(code);
        assert!(s.intrinsic > score_cognitive_load("fn y() {}").intrinsic);
    }

    #[test]
    fn comments_and_blank_lines_raise_extraneous() {
        let noisy = "// head\n\n// more\nfn z() {}\n";
        let plain = "fn z() {}\n";
        assert!(score_cognitive_load(noisy).extraneous > score_cognitive_load(plain).extraneous);
    }

    #[test]
    fn types_and_algo_boost_germane() {
        let api = "fn k(a: HashMap<String, Vec<MyDto>>) { a.sort(); }\n";
        let s = score_cognitive_load(api);
        assert!(s.germane > 0.15);
    }

    #[test]
    fn recommendation_prefers_entropy_on_noise() {
        let wall = "// ...\n".repeat(40);
        let s = score_cognitive_load(&(wall + "\nfn q() {}\n"));
        assert!(
            s.recommendation.contains("entropy") || s.recommendation.contains("aggressive"),
            "{}",
            s.recommendation
        );
    }
}
