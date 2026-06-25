use regex::Regex;

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

#[derive(Debug, Clone)]
pub struct Signature {
    pub kind: &'static str,
    pub name: String,
    pub params: String,
    pub return_type: String,
    pub is_async: bool,
    pub is_exported: bool,
    pub indent: usize,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub minhash: Option<[u32; 64]>,
}

/// Exports not already named by `key_sigs`. Map-mode renderers list the API
/// with full signatures + line ranges; repeating those same names in an
/// `exports:` line is pure redundancy. Callers surface only the exports the API
/// does not already cover (e.g. re-exports or const/type aliases that aren't
/// captured as signatures), keeping map lossless while dropping duplicates
/// (#361). Shared by the MCP, CLI, and benchmark map renderers.
#[must_use]
pub fn exports_not_in_signatures<'a>(
    exports: &'a [String],
    key_sigs: &[&Signature],
) -> Vec<&'a str> {
    let api_names: std::collections::HashSet<&str> =
        key_sigs.iter().map(|s| s.name.as_str()).collect();
    exports
        .iter()
        .map(String::as_str)
        .filter(|e| !api_names.contains(e))
        .collect()
}

impl Signature {
    #[must_use]
    pub fn no_span() -> Self {
        Self {
            kind: "",
            name: String::new(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: false,
            indent: 0,
            start_line: None,
            end_line: None,
            minhash: None,
        }
    }

    /// Plain, self-describing notation (GL #580): real keywords (`class`,
    /// `trait`, `pub`) instead of abbreviations, so a vanilla agent without
    /// injected rules reads the output correctly. Keyword tokens cost the
    /// same as (or less than) the old `cl`/`⊛` forms.
    #[must_use]
    pub fn to_compact(&self) -> String {
        let export = if self.is_exported { "pub " } else { "" };
        let async_prefix = if self.is_async { "async " } else { "" };

        match self.kind {
            "fn" | "method" => {
                let ret = if self.return_type.is_empty() {
                    String::new()
                } else {
                    format!(" → {}", self.return_type)
                };
                let indent = " ".repeat(self.indent);
                format!(
                    "{indent}fn {async_prefix}{export}{}({}){}",
                    self.name, self.params, ret
                )
            }
            "const" | "let" | "var" => {
                let ty = if self.return_type.is_empty() {
                    String::new()
                } else {
                    format!(":{}", self.return_type)
                };
                format!("{} {export}{}{ty}", self.kind, self.name)
            }
            // `kind` is already the source-language keyword (class, struct,
            // interface, trait, type, enum) — use it verbatim.
            _ => format!("{} {export}{}", self.kind, self.name),
        }
    }

    #[must_use]
    pub fn to_tdd(&self) -> String {
        let vis = if self.is_exported { "+" } else { "-" };
        let a = if self.is_async { "~" } else { "" };

        match self.kind {
            "fn" | "method" => {
                let ret = if self.return_type.is_empty() {
                    String::new()
                } else {
                    format!("→{}", compact_type(&self.return_type))
                };
                let params = tdd_params(&self.params);
                let indent = if self.indent > 0 { " " } else { "" };
                format!("{indent}{a}λ{vis}{}({params}){ret}", self.name)
            }
            "class" | "struct" => format!("§{vis}{}", self.name),
            "interface" | "trait" => format!("∂{vis}{}", self.name),
            "type" => format!("τ{vis}{}", self.name),
            "enum" => format!("ε{vis}{}", self.name),
            "const" | "let" | "var" => {
                let ty = if self.return_type.is_empty() {
                    String::new()
                } else {
                    format!(":{}", compact_type(&self.return_type))
                };
                format!("ν{vis}{}{ty}", self.name)
            }
            _ => format!(
                "{}{vis}{}",
                self.kind.chars().next().unwrap_or('?'),
                self.name
            ),
        }
    }

    /// Compact ` @Lstart[-end]` suffix for navigation-focused modes.
    /// Returns an empty string when the span is unknown, so compression-first
    /// modes that render the base `to_compact`/`to_tdd` stay byte-identical.
    #[must_use]
    pub fn line_suffix(&self) -> String {
        match (self.start_line, self.end_line) {
            (Some(start), Some(end)) if start > 0 && end > start => format!(" @L{start}-{end}"),
            (Some(start), _) if start > 0 => format!(" @L{start}"),
            _ => String::new(),
        }
    }

    /// `to_compact` plus a line-span suffix. Reserved for navigation modes
    /// (`map`/`signatures`) where locating code outweighs the few extra tokens.
    #[must_use]
    pub fn to_compact_located(&self) -> String {
        format!("{}{}", self.to_compact(), self.line_suffix())
    }

    /// `to_tdd` plus a line-span suffix. Reserved for navigation modes.
    #[must_use]
    pub fn to_tdd_located(&self) -> String {
        format!("{}{}", self.to_tdd(), self.line_suffix())
    }
}

/// One-line legend for TDD symbol notation (GL #580): outputs must be
/// self-describing for vanilla agents without injected rules. Only the
/// symbols actually present in `sigs` are explained, keeping the line well
/// under the 15-token budget for typical files. Empty when nothing applies.
#[must_use]
pub fn tdd_legend<'a>(sigs: &[&'a Signature]) -> String {
    if sigs.is_empty() {
        return String::new();
    }
    let mut parts: Vec<&str> = Vec::new();
    let has = |pred: &dyn Fn(&'a Signature) -> bool| sigs.iter().any(|s| pred(s));

    if has(&|s| matches!(s.kind, "fn" | "method")) {
        parts.push("λ=fn");
    }
    if has(&|s| matches!(s.kind, "class" | "struct")) {
        parts.push("§=class");
    }
    if has(&|s| matches!(s.kind, "interface" | "trait")) {
        parts.push("∂=trait");
    }
    if has(&|s| s.kind == "type") {
        parts.push("τ=type");
    }
    if has(&|s| s.kind == "enum") {
        parts.push("ε=enum");
    }
    if has(&|s| matches!(s.kind, "const" | "let" | "var")) {
        parts.push("ν=val");
    }
    if has(&|s| s.is_exported) {
        parts.push("+=pub");
    }
    if has(&|s| s.is_async) {
        parts.push("~=async");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("[{}]", parts.join(" "))
    }
}

fn fn_re() -> &'static Regex {
    static_regex!(
        r"^(\s*)(export\s+)?(async\s+)?function\s+(\w+)\s*(?:<[^>]*>)?\s*\(([^)]*)\)(?:\s*:\s*([^\{]+))?\s*\{?"
    )
}

fn class_re() -> &'static Regex {
    static_regex!(r"^(\s*)(export\s+)?(abstract\s+)?class\s+(\w+)")
}

fn iface_re() -> &'static Regex {
    static_regex!(r"^(\s*)(export\s+)?interface\s+(\w+)")
}

fn type_re() -> &'static Regex {
    static_regex!(r"^(\s*)(export\s+)?type\s+(\w+)")
}

fn const_re() -> &'static Regex {
    static_regex!(r"^(\s*)(export\s+)?(const|let|var)\s+(\w+)(?:\s*:\s*(\w+))?")
}

fn rust_fn_re() -> &'static Regex {
    static_regex!(
        r"^(\s*)(pub\s+)?(async\s+)?fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(([^)]*)\)(?:\s*->\s*([^\{]+))?\s*\{?"
    )
}

fn rust_struct_re() -> &'static Regex {
    static_regex!(r"^(\s*)(pub\s+)?struct\s+(\w+)")
}

fn rust_enum_re() -> &'static Regex {
    static_regex!(r"^(\s*)(pub\s+)?enum\s+(\w+)")
}

fn rust_trait_re() -> &'static Regex {
    static_regex!(r"^(\s*)(pub\s+)?trait\s+(\w+)")
}

fn rust_impl_re() -> &'static Regex {
    static_regex!(r"^(\s*)impl\s+(?:(\w+)\s+for\s+)?(\w+)")
}

use std::sync::atomic::{AtomicU64, Ordering};

static TREE_SITTER_HITS: AtomicU64 = AtomicU64::new(0);
static REGEX_FALLBACK_HITS: AtomicU64 = AtomicU64::new(0);

/// Returns (`tree_sitter_hits`, `regex_fallback_hits`) since process start.
pub fn signature_backend_stats() -> (u64, u64) {
    (
        TREE_SITTER_HITS.load(Ordering::Relaxed),
        REGEX_FALLBACK_HITS.load(Ordering::Relaxed),
    )
}

pub fn extract_signatures(content: &str, file_ext: &str) -> Vec<Signature> {
    #[cfg(feature = "tree-sitter")]
    {
        // A successful parse that yields no signatures (`Some(vec![])`) means the
        // tree-sitter query set has a gap for this file's constructs. Fall through
        // to the regex extractors instead of returning the empty result, which
        // would otherwise suppress signatures the regex fallback can still
        // recover (e.g. constructs the query does not capture).
        if let Some(sigs) = super::signatures_ts::extract_signatures_ts(content, file_ext)
            && !sigs.is_empty()
        {
            TREE_SITTER_HITS.fetch_add(1, Ordering::Relaxed);
            return sigs;
        }
    }

    REGEX_FALLBACK_HITS.fetch_add(1, Ordering::Relaxed);
    match file_ext {
        "rs" => extract_rust_signatures(content),
        "ts" | "tsx" | "js" | "jsx" | "svelte" | "vue" => extract_ts_signatures(content),
        "py" => extract_python_signatures(content),
        "go" => extract_go_signatures(content),
        _ => extract_generic_signatures(content),
    }
}

pub fn extract_file_map(path: &str, content: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("rs");
    let dep_info = super::deps::extract_deps(content, ext);
    let sigs = extract_signatures(content, ext);
    let mut parts = Vec::new();
    if !dep_info.imports.is_empty() {
        parts.push(dep_info.imports.join(","));
    }
    let key_sigs: Vec<String> = sigs
        .iter()
        .filter(|s| s.is_exported || s.indent == 0)
        .map(Signature::to_compact_located)
        .collect();
    if !key_sigs.is_empty() {
        parts.push(key_sigs.join("\n"));
    }
    parts.join("\n")
}

fn extract_ts_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        if let Some(caps) = fn_re().captures(line) {
            let indent = caps.get(1).map_or(0, |m| m.as_str().len());
            sigs.push(Signature {
                kind: if indent > 0 { "method" } else { "fn" },
                name: caps[4].to_string(),
                params: compact_params(&caps[5]),
                return_type: caps
                    .get(6)
                    .map_or(String::new(), |m| m.as_str().trim().to_string()),
                is_async: caps.get(3).is_some(),
                is_exported: caps.get(2).is_some(),
                indent: if indent > 0 { 2 } else { 0 },
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = class_re().captures(line) {
            sigs.push(Signature {
                kind: "class",
                name: caps[4].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = iface_re().captures(line) {
            sigs.push(Signature {
                kind: "interface",
                name: caps[3].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = type_re().captures(line) {
            sigs.push(Signature {
                kind: "type",
                name: caps[3].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = const_re().captures(line)
            && caps.get(2).is_some()
        {
            sigs.push(Signature {
                kind: "const",
                name: caps[4].to_string(),
                params: String::new(),
                return_type: caps
                    .get(5)
                    .map_or(String::new(), |m| m.as_str().to_string()),
                is_async: false,
                is_exported: true,
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        }
    }

    sigs
}

fn extract_rust_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }

        if let Some(caps) = rust_fn_re().captures(line) {
            let indent = caps.get(1).map_or(0, |m| m.as_str().len());
            sigs.push(Signature {
                kind: if indent > 0 { "method" } else { "fn" },
                name: caps[4].to_string(),
                params: compact_params(&caps[5]),
                return_type: caps
                    .get(6)
                    .map_or(String::new(), |m| m.as_str().trim().to_string()),
                is_async: caps.get(3).is_some(),
                is_exported: caps.get(2).is_some(),
                indent: if indent > 0 { 2 } else { 0 },
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = rust_struct_re().captures(line) {
            sigs.push(Signature {
                kind: "struct",
                name: caps[3].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = rust_enum_re().captures(line) {
            sigs.push(Signature {
                kind: "enum",
                name: caps[3].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = rust_trait_re().captures(line) {
            sigs.push(Signature {
                kind: "trait",
                name: caps[3].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps.get(2).is_some(),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = rust_impl_re().captures(line) {
            let trait_name = caps.get(2).map(|m| m.as_str());
            let type_name = &caps[3];
            let name = if let Some(t) = trait_name {
                format!("{t} for {type_name}")
            } else {
                type_name.to_string()
            };
            sigs.push(Signature {
                kind: "class",
                name,
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: false,
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        }
    }

    sigs
}

fn extract_python_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    let py_fn = static_regex!(r"^(\s*)(async\s+)?def\s+(\w+)\s*\(([^)]*)\)(?:\s*->\s*(\w+))?");
    let py_class = static_regex!(r"^(\s*)class\s+(\w+)");

    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        if let Some(caps) = py_fn.captures(line) {
            let indent = caps.get(1).map_or(0, |m| m.as_str().len());
            sigs.push(Signature {
                kind: if indent > 0 { "method" } else { "fn" },
                name: caps[3].to_string(),
                params: compact_params(&caps[4]),
                return_type: caps
                    .get(5)
                    .map_or(String::new(), |m| m.as_str().to_string()),
                is_async: caps.get(2).is_some(),
                is_exported: !caps[3].starts_with('_'),
                indent: if indent > 0 { 2 } else { 0 },
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = py_class.captures(line) {
            sigs.push(Signature {
                kind: "class",
                name: caps[2].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: !caps[2].starts_with('_'),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        }
    }

    sigs
}

fn extract_go_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    let go_fn = static_regex!(
        r"^func\s+(?:\((\w+)\s+\*?(\w+)\)\s+)?(\w+)\s*\(([^)]*)\)(?:\s*(?:\(([^)]*)\)|(\w+)))?\s*\{"
    );
    let go_type = static_regex!(r"^type\s+(\w+)\s+(struct|interface)");

    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        if let Some(caps) = go_fn.captures(line) {
            let is_method = caps.get(2).is_some();
            sigs.push(Signature {
                kind: if is_method { "method" } else { "fn" },
                name: caps[3].to_string(),
                params: compact_params(&caps[4]),
                return_type: caps
                    .get(5)
                    .or(caps.get(6))
                    .map_or(String::new(), |m| m.as_str().to_string()),
                is_async: false,
                is_exported: caps[3].starts_with(char::is_uppercase),
                indent: if is_method { 2 } else { 0 },
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = go_type.captures(line) {
            sigs.push(Signature {
                kind: if &caps[2] == "struct" {
                    "struct"
                } else {
                    "interface"
                },
                name: caps[1].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: caps[1].starts_with(char::is_uppercase),
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        }
    }

    sigs
}

pub(crate) fn compact_params(params: &str) -> String {
    if params.trim().is_empty() {
        return String::new();
    }
    params
        .split(',')
        .map(|p| {
            let p = p.trim();
            if let Some((name, ty)) = p.split_once(':') {
                let name = name.trim();
                let ty = ty.trim();
                let short = match ty {
                    "string" | "String" | "&str" | "str" => ":s",
                    "number" | "i32" | "i64" | "u32" | "u64" | "usize" | "f32" | "f64" => ":n",
                    "boolean" | "bool" => ":b",
                    _ => return format!("{name}:{ty}"),
                };
                format!("{name}{short}")
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn compact_type(ty: &str) -> String {
    match ty.trim() {
        "String" | "string" | "&str" | "str" => "s".to_string(),
        "bool" | "boolean" => "b".to_string(),
        "i32" | "i64" | "u32" | "u64" | "usize" | "f32" | "f64" | "number" => "n".to_string(),
        "void" | "()" => "∅".to_string(),
        other => {
            if other.starts_with("Vec<") || other.starts_with("Array<") {
                let inner = other
                    .trim_start_matches("Vec<")
                    .trim_start_matches("Array<")
                    .trim_end_matches('>');
                format!("[{}]", compact_type(inner))
            } else if other.starts_with("Option<") || other.starts_with("Maybe<") {
                let inner = other
                    .trim_start_matches("Option<")
                    .trim_start_matches("Maybe<")
                    .trim_end_matches('>');
                format!("?{}", compact_type(inner))
            } else if other.starts_with("Result<") {
                "R".to_string()
            } else if other.starts_with("impl ") {
                other.trim_start_matches("impl ").to_string()
            } else {
                other.to_string()
            }
        }
    }
}

fn tdd_params(params: &str) -> String {
    if params.trim().is_empty() {
        return String::new();
    }
    params
        .split(',')
        .map(|p| {
            let p = p.trim();
            if p.starts_with('&') {
                let rest = p.trim_start_matches("&mut ").trim_start_matches('&');
                if let Some((name, ty)) = rest.split_once(':') {
                    format!("&{}:{}", name.trim(), compact_type(ty))
                } else {
                    p.to_string()
                }
            } else if let Some((name, ty)) = p.split_once(':') {
                format!("{}:{}", name.trim(), compact_type(ty))
            } else if p == "self" || p == "&self" || p == "&mut self" {
                "⊕".to_string()
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn extract_generic_signatures(content: &str) -> Vec<Signature> {
    let re_func = static_regex!(
        r"^\s*(?:(?:public|private|protected|static|async|abstract|virtual|override|final|def|func|fun|fn)\s+)+(\w+)\s*\("
    );
    let re_class = static_regex!(
        r"^\s*(?:(?:public|private|protected|abstract|final|sealed|partial)\s+)*(?:class|struct|enum|interface|trait|module|object|record)\s+(\w+)"
    );

    let mut sigs = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }
        if let Some(caps) = re_class.captures(trimmed) {
            sigs.push(Signature {
                kind: "type",
                name: caps[1].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: true,
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        } else if let Some(caps) = re_func.captures(trimmed) {
            sigs.push(Signature {
                kind: "fn",
                name: caps[1].to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: trimmed.contains("async"),
                is_exported: true,
                indent: 0,
                start_line: Some(line_no),
                end_line: Some(line_no),
                minhash: None,
            });
        }
    }
    sigs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fn() -> Signature {
        Signature {
            kind: "fn",
            name: "run".to_string(),
            params: "id:usize".to_string(),
            return_type: "bool".to_string(),
            is_async: false,
            is_exported: true,
            indent: 0,
            start_line: None,
            end_line: None,
            minhash: None,
        }
    }

    #[test]
    fn line_suffix_formats_known_spans() {
        let mut sig = sample_fn();
        assert_eq!(sig.line_suffix(), "");

        sig.start_line = Some(42);
        sig.end_line = Some(42);
        assert_eq!(sig.line_suffix(), " @L42");

        sig.end_line = Some(57);
        assert_eq!(sig.line_suffix(), " @L42-57");
    }

    #[test]
    fn base_renderers_stay_suffix_free() {
        // Compression-first modes must never pay for line ranges, even when
        // the span is known.
        let mut sig = sample_fn();
        sig.start_line = Some(3);
        sig.end_line = Some(9);
        assert_eq!(sig.to_compact(), "fn pub run(id:usize) → bool");
        assert_eq!(sig.to_tdd(), "λ+run(id:n)→b");
    }

    #[test]
    fn located_renderers_append_line_suffix() {
        let mut sig = sample_fn();
        // Unknown span → identical to the base renderer.
        assert_eq!(sig.to_compact_located(), "fn pub run(id:usize) → bool");
        assert_eq!(sig.to_tdd_located(), "λ+run(id:n)→b");

        sig.start_line = Some(3);
        sig.end_line = Some(5);
        assert_eq!(
            sig.to_compact_located(),
            "fn pub run(id:usize) → bool @L3-5"
        );
        assert_eq!(sig.to_tdd_located(), "λ+run(id:n)→b @L3-5");
    }

    #[test]
    fn plain_notation_is_self_describing() {
        // GL #580: plain mode must read like source keywords, no decoder ring.
        let mut sig = sample_fn();
        sig.kind = "struct";
        assert_eq!(sig.to_compact(), "struct pub run");
        sig.kind = "trait";
        assert_eq!(sig.to_compact(), "trait pub run");
        sig.kind = "enum";
        sig.is_exported = false;
        assert_eq!(sig.to_compact(), "enum run");
        sig.kind = "const";
        sig.return_type = "u32".to_string();
        assert_eq!(sig.to_compact(), "const run:u32");
    }

    #[test]
    fn tdd_legend_explains_only_present_symbols() {
        // GL #580: symbol outputs carry their own minimal legend.
        let f = sample_fn();
        let mut s = sample_fn();
        s.kind = "struct";
        s.is_exported = false;

        let legend = tdd_legend(&[&f, &s]);
        assert_eq!(legend, "[λ=fn §=class +=pub]");
        // Stays comfortably under the 15-token budget for a typical file.
        assert!(crate::core::tokens::count_tokens(&legend) <= 15, "{legend}");

        assert_eq!(tdd_legend(&[]), "");
    }

    #[test]
    fn regex_fallback_assigns_declaration_line_spans() {
        let src = "\npublic class Service {}\n\npublic fn run() {\n}\n";
        let sigs = extract_generic_signatures(src);

        let service = sigs.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(service.start_line, Some(2));
        assert_eq!(service.end_line, Some(2));

        let run = sigs.iter().find(|s| s.name == "run").unwrap();
        assert_eq!(run.start_line, Some(4));
        assert_eq!(run.end_line, Some(4));
    }
}
