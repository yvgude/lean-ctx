#[cfg(test)]
pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
    // Held from before the snapshot until after the restore below: the
    // struct's own `Drop` runs before this field is released.
    _lock: crate::core::data_dir::TestEnvGuard,
}

#[cfg(test)]
impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let lock = crate::core::data_dir::test_env_lock();
        let previous = std::env::var_os(key);
        crate::test_env::set_var(key, value);
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            crate::test_env::set_var(self.key, previous);
        } else {
            crate::test_env::remove_var(self.key);
        }
    }
}
