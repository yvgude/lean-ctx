// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::SigningKey;
use lean_ctx::core::a2a::task::{TASK_ACTION_GET, TaskAuthorityError, TaskControlDescriptorV1};

fn control(lifetime: Duration) -> TaskControlDescriptorV1 {
    let issued_at = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("fixed valid timestamp")
        .with_timezone(&Utc);
    let mut descriptor = TaskControlDescriptorV1::new(
        "agent-a",
        "server-b",
        "tenant-a",
        "project-a",
        TASK_ACTION_GET,
        "task-1",
        &format!("sha256:{}", "a".repeat(64)),
        issued_at,
        issued_at + lifetime,
        "nonce-1",
        None,
        "grant-get",
        "key-a",
    );
    descriptor.sign(&SigningKey::from_bytes(&[7; 32]));
    descriptor
}

#[test]
fn control_accepts_exact_fifteen_minute_lifetime() {
    assert_eq!(control(Duration::minutes(15)).validate_shape(), Ok(()));
}

#[test]
fn control_rejects_even_subsecond_lifetime_overrun() {
    assert_eq!(
        control(Duration::minutes(15) + Duration::nanoseconds(1)).validate_shape(),
        Err(TaskAuthorityError::MalformedBounds)
    );
}
