use regex::Regex;

macro_rules! static_regex {
    ($pattern:expr) => {{
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
}

impl Signature {
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
        }
    }

    pub fn to_compact(&self) -> String {
        let export = if self.is_exported { "⊛ " } else { "" };
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
            "class" | "struct" => format!("cl {export}{}", self.name),
            "interface" | "trait" => format!("if {export}{}", self.name),
            "type" => format!("ty {export}{}", self.name),
            "enum" => format!("en {export}{}", self.name),
            "const" | "let" | "var" => {
                let ty = if self.return_type.is_empty() {
                    String::new()
                } else {
                    format!(":{}", self.return_type)
                };
                format!("val {export}{}{ty}", self.name)
            }
            _ => format!("{} {}", self.kind, self.name),
        }
    }

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

pub fn extract_signatures(content: &str, file_ext: &str) -> Vec<Signature> {
    #[cfg(feature = "tree-sitter")]
    {
        if let Some(sigs) = super::signatures_ts::extract_signatures_ts(content, file_ext) {
            return sigs;
        }
    }

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
        .map(Signature::to_compact)
        .collect();
    if !key_sigs.is_empty() {
        parts.push(key_sigs.join("\n"));
    }
    parts.join("\n")
}

fn extract_ts_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();

    for line in content.lines() {
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
                ..Signature::no_span()
            });
        } else if let Some(caps) = const_re().captures(line) {
            if caps.get(2).is_some() {
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
                    ..Signature::no_span()
                });
            }
        }
    }

    sigs
}

fn extract_rust_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();

    for line in content.lines() {
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
                ..Signature::no_span()
            });
        }
    }

    sigs
}

fn extract_python_signatures(content: &str) -> Vec<Signature> {
    let mut sigs = Vec::new();
    let py_fn = static_regex!(r"^(\s*)(async\s+)?def\s+(\w+)\s*\(([^)]*)\)(?:\s*->\s*(\w+))?");
    let py_class = static_regex!(r"^(\s*)class\s+(\w+)");

    for line in content.lines() {
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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

    for line in content.lines() {
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
                ..Signature::no_span()
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
                ..Signature::no_span()
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
    for line in content.lines() {
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
                ..Signature::no_span()
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
                ..Signature::no_span()
            });
        }
    }
    sigs
}
