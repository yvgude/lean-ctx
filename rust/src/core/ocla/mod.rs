//! Open Context & Token Lifecycle Architecture (OCLA) public OSS contract.
//!
//! OCLA exposes local, provider-neutral control points. Implementations remain
//! in the engine or an OSS extension; commercial systems may consume this
//! versioned boundary but must never become a data-plane dependency.

pub mod traits;
pub mod types;
pub mod wire;

pub use traits::*;
pub use types::*;
