//! Predictive Context Prefetch (F2) — trajectory-based preloading.
//!
//! Unifies predictive prefetch, FEP prefetch, and active inference into a
//! coherent prefetch pipeline with trajectory prediction.

pub mod preloader;
pub mod trajectory;
pub mod warming;

pub use preloader::PrefetchPlan;
#[cfg(test)]
pub use preloader::build_prefetch_plan;
pub use preloader::{is_prefetch_prediction, plan_after_triage, record_file_read};
pub use trajectory::FileTrajectory;
pub use warming::{skipped_count, warm_predictions, warmed_count};
