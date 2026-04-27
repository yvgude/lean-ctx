pub(super) fn extract_git_subcommand(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    while let Some(tok) = tokens.next() {
        let base = tok.rsplit('/').next().unwrap_or(tok);
        if base == "git" {
            let mut skip_next = false;
            for arg in tokens {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if arg == "-C" || arg == "-c" || arg == "--git-dir" || arg == "--work-tree" {
                    skip_next = true;
                    continue;
                }
                if arg.starts_with('-') {
                    continue;
                }
                return Some(arg);
            }
            return None;
        }
    }
    None
}
