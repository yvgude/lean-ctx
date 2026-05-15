use crate::core::protocol;

pub fn shell_savings_footer(output: &str, original: usize, compressed: usize) -> String {
    protocol::append_savings(output, original, compressed)
}
