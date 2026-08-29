/// Strip insignificant whitespace outside string literals (spec §8).
pub(crate) fn compact_json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match ch {
                '\\' => {
                    if let Some(esc) = chars.next() {
                        out.push(esc);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
        } else {
            match ch {
                '"' => {
                    in_string = true;
                    out.push(ch);
                }
                ' ' | '\t' | '\n' | '\r' => {}
                _ => out.push(ch),
            }
        }
    }
    out
}
