use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LeanCtxError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("lock error: {0}")]
    Lock(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("plugin error: {0}")]
    Plugin(String),

    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Cannot determine config path")]
    MissingPath,

    #[error("Unknown config key: {key}")]
    UnknownKey { key: String },

    #[error("Cannot read config: {source}")]
    Read {
        #[source]
        source: std::io::Error,
    },

    #[error("Config parse error: {source}")]
    ParseToml {
        #[source]
        source: toml::de::Error,
    },

    #[error("Invalid value for '{key}': {message}")]
    InvalidValue { key: String, message: String },

    #[error("Error saving config: {source}")]
    Save {
        #[source]
        source: Box<LeanCtxError>,
    },

    #[error("Expected bool (true/false), got: {value}")]
    ExpectedBool { value: String },

    #[error("Expected integer, got: {value}")]
    ExpectedInteger { value: String },

    #[error("Expected unsigned integer, got: {value}")]
    ExpectedUnsignedInteger { value: String },

    #[error("Expected number, got: {value}")]
    ExpectedNumber { value: String },

    #[error("Invalid value '{value}'. Allowed: {allowed}")]
    InvalidEnumValue { value: String, allowed: String },

    #[error("Cannot set table '{value}' via CLI. Edit config.toml directly.")]
    CannotSetTable { value: String },

    #[error(
        "Cannot set '{key}': '{part}' already holds a non-table value in config.toml. Fix or remove that key first."
    )]
    NonTableParent { key: String, part: String },

    #[error("{0}")]
    Message(String),
}

#[derive(Error, Debug)]
pub enum DispatchError {
    #[error("path resolution failed: {message}")]
    PathResolution { message: String },

    #[error("{message}")]
    Tool { message: String },
}

#[derive(Error, Debug)]
pub enum ShellError {
    #[error("{message}")]
    Blocked { message: String },
}

impl From<String> for ShellError {
    fn from(message: String) -> Self {
        Self::Blocked { message }
    }
}

impl From<&str> for ShellError {
    fn from(message: &str) -> Self {
        Self::Blocked {
            message: message.to_string(),
        }
    }
}

impl ShellError {
    pub fn message(&self) -> &str {
        match self {
            Self::Blocked { message } => message,
        }
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.message().contains(needle)
    }

    pub fn lines(&self) -> std::str::Lines<'_> {
        self.message().lines()
    }
}

#[derive(Error, Debug)]
pub enum PathJailError {
    #[error("protected runtime context access denied: {reason}")]
    ProtectedContext { reason: String },

    #[error("path contains null byte")]
    NullByte,

    #[error("path does not exist and has no existing ancestor: {path}")]
    NoExistingAncestor { path: PathBuf },

    #[error("path escapes project root: {path} (root: {root}){hint}")]
    EscapesRoot {
        path: PathBuf,
        root: PathBuf,
        hint: String,
    },

    #[error("post-canonicalize jail escape detected: {path} resolves to {resolved}")]
    PostCanonicalizeEscape { path: PathBuf, resolved: PathBuf },

    #[error("symlink not allowed in jailed path: {path}")]
    Symlink { path: PathBuf },
}

impl From<String> for ConfigError {
    fn from(message: String) -> Self {
        ConfigError::Message(message)
    }
}

impl From<&str> for ConfigError {
    fn from(message: &str) -> Self {
        ConfigError::Message(message.to_string())
    }
}

impl From<toml::de::Error> for LeanCtxError {
    fn from(e: toml::de::Error) -> Self {
        LeanCtxError::Config(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, LeanCtxError>;
