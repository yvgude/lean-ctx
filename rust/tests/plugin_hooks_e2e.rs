//! End-to-end proof that a plugin observes a real `ctx_read` event.
//!
//! NOTE: The refactored ctx_read::read() is pure and no longer fires plugin
//! hooks. The pre_read hook mechanism was intentionally decoupled. This test
//! is preserved as a stub to document the removed integration point.
//!
//! If plugin hooks for reads are re-added in the future, restore the test
//! using ctx_read::read() with ReadMode::Full(None).
