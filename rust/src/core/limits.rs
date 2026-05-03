pub const DEFAULT_MAX_READ_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_SHELL_BYTES: usize = 2 * 1024 * 1024;

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
}

pub fn max_read_bytes() -> usize {
    env_usize("LCTX_MAX_READ_BYTES")
        .or_else(|| env_usize("LEAN_CTX_MAX_READ_BYTES"))
        .unwrap_or(DEFAULT_MAX_READ_BYTES)
        .max(1024)
}

pub fn max_shell_bytes() -> usize {
    env_usize("LCTX_MAX_SHELL_BYTES")
        .or_else(|| env_usize("LEAN_CTX_MAX_SHELL_BYTES"))
        .unwrap_or(DEFAULT_MAX_SHELL_BYTES)
        .max(1024)
}
