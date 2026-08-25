use super::types::Profile;

/// Pure, borrowed view of deterministic Engine mechanism configuration.
///
/// The legacy `Profile` remains the loader and compatibility adapter while
/// Product policy fields stay outside this Engine-facing seam.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineConfig<'a> {
    pub(crate) read: EngineReadConfig<'a>,
    pub(crate) compression: EngineCompressionConfig<'a>,
}

/// Read mechanism values only; reuse policy such as `prefer_cache` is excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EngineReadConfig<'a> {
    pub(crate) default_mode: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EngineCompressionConfig<'a> {
    pub(crate) crp_mode: &'a str,
}

impl Profile {
    pub(crate) fn engine_config(&self) -> EngineConfig<'_> {
        EngineConfig {
            read: EngineReadConfig {
                default_mode: self.read.default_mode_effective(),
            },
            compression: EngineCompressionConfig {
                crp_mode: self.compression.crp_mode_effective(),
            },
        }
    }
}
