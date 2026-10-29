/// Strip insignificant whitespace outside string literals (spec §8).
pub(crate) fn compact_json_text(text: &str) -> String {
    crate::core::context_package::integrity::compact_json_text(text)
}
