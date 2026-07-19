//! Built-in OCLA trait implementations (P3).
//!
//! Each built-in wraps existing lean-ctx modules behind the OCLA trait
//! interface. Behavior is identical to the wrapped code — the trait boundary
//! enables future swapping, testing, and adoption tracking.

pub mod analyzer;
pub mod experiment;
pub mod intent_classifier;
pub mod outcome_tracker;
pub mod tuner;
