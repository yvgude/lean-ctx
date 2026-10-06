pub mod delivery_authority;
pub mod delivery_delegation;
pub(crate) mod policy_file;

const MAX_TASK_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TASKS: usize = 10_000;
pub const MAX_TASK_DESCRIPTOR_BYTES: usize = 256 * 1024;
const MAX_DESCRIPTOR_STRING_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 64 * 1024;
const MAX_ARTIFACT_REFS: usize = 64;
const MAX_IDEMPOTENCY_RECORDS: usize = 10_000;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAX_TASK_LIFETIME_SECONDS: i64 = 24 * 60 * 60;
const MAX_POLICY_LIFETIME_SECONDS: i64 = 366 * 24 * 60 * 60;
const MAX_TRUST_ENTRIES: usize = 1_024;
/// Accepted clock difference between the signing peer and this receiver.
const MAX_AUTHORITY_CLOCK_SKEW_SECONDS: i64 = 60;
/// An idempotency record is only useful while a descriptor bearing that key can
/// still verify, so retention is bounded by the maximum descriptor lifetime
/// plus the accepted clock skew. Expired records are pruned instead of growing
/// until the hard cap turns every later delivery into a store error.
const IDEMPOTENCY_RETENTION_SECONDS: i64 =
    MAX_TASK_LIFETIME_SECONDS + MAX_AUTHORITY_CLOCK_SKEW_SECONDS;
/// Domain tag prefixed to the descriptor signing transcript so a descriptor
/// signature can never be replayed as a signature over another artifact that
/// happens to canonicalize to the same bytes.
const TASK_DESCRIPTOR_SIGNING_DOMAIN: &[u8] = b"leanctx.a2a.task.descriptor.sig.v1\0";
/// Domain tag for the control transcript. Distinct from the descriptor domain
/// so a send signature can never be replayed as a `tasks/get` or `tasks/cancel`
/// signature, nor the reverse, whatever the canonical bodies look like.
const TASK_CONTROL_SIGNING_DOMAIN: &[u8] = b"leanctx.a2a.task.control.sig.v1\0";
/// A control operation carries no payload and is answered immediately, so its
/// validity window is bounded far tighter than a task's — strictly stricter
/// than `MAX_TASK_LIFETIME_SECONDS`, never looser.
const MAX_TASK_CONTROL_LIFETIME_SECONDS: i64 = 15 * 60;
const MAX_CANCEL_REASON_BYTES: usize = 512;
const MAX_CONTROL_NONCE_BYTES: usize = MAX_IDEMPOTENCY_KEY_BYTES;
const DIGEST_PREFIX: &str = "sha256:";
const DIGEST_HEX_LEN: usize = 64;
const MAX_ID_BYTES: usize = 256;
const MAX_MESSAGES: usize = 1_024;
const MAX_PARTS: usize = 1_024;
const MAX_PARTS_PER_MESSAGE: usize = 256;
const MAX_HISTORY: usize = 2_048;
const MAX_METADATA_ENTRIES: usize = 256;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_METADATA_VALUE_BYTES: usize = 64 * 1024;

pub const TASK_DESCRIPTOR_VERSION: u32 = 1;
pub const TASK_STATUS_VERSION: u32 = 1;
pub const ARTIFACT_REF_VERSION: u32 = 1;
pub const CAPABILITY_GRANT_REF_VERSION: u32 = 1;
pub const TASK_AUTHORITY_CONFIG_VERSION: u32 = 1;
pub const TASK_ACTION_SEND: &str = "tasks/send";
pub const TASK_ACTION_GET: &str = "tasks/get";
pub const TASK_ACTION_CANCEL: &str = "tasks/cancel";
pub const TASK_CONTROL_DESCRIPTOR_VERSION: u32 = 1;

mod authority;
mod model;
mod store;

pub use authority::{
    AgentArtifactRefV1, CapabilityGrantRefV1, TaskAuthorityConfigV1, TaskAuthorityError,
    TaskAuthorityExpectationV1, TaskCapabilityGrantV1, TaskControlDescriptorV1, TaskDescriptorV1,
    TaskPeerTrustV1, TaskScopeV1,
};
pub use model::{Task, TaskMessage, TaskPart, TaskState, TaskStatusV1, TaskTransition};
pub use store::{RemoteTaskMaterialization, TaskStore};

use authority::{AuthorityClaimV1, authorize, validate_digest, validate_identifier};

#[cfg(test)]
#[path = "task/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "task/authority_tests.rs"]
mod authority_tests;
