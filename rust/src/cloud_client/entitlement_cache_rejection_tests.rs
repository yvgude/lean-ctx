// SPDX-License-Identifier: Apache-2.0

#[test]
fn invalid_vector_response_cannot_resurrect_the_cached_paid_entitlement() {
    let fixture = vector_fixture();
    let permit = authorize_at(&fixture.paths, "cloud_sync", VECTOR_ACTIVE_NOW)
        .ok()
        .unwrap();
    let tampered = vector_with_one_flipped_byte(b"\"seats\":23", b'4');
    let result = refresh_with(
        &fixture.paths,
        || VECTOR_ACTIVE_NOW,
        |_| FetchOutcome::Envelope(tampered.clone()),
    );
    assert_eq!(result.verification_status, "denied");
    assert_eq!(result.plan, Plan::Community);
    // Invalid live success is an authenticated rejection of the replayable cache.
    assert!(std::fs::read(&fixture.paths.cache).unwrap().is_empty());
    assert!(permit.ensure_valid_at(VECTOR_ACTIVE_NOW).is_err());
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "denied"
    );
    // A later outage may not restore the old paid grace either; the persisted
    // receipt still denies after this process's memory is cleared.
    assert_eq!(
        refresh_with(
            &fixture.paths,
            || VECTOR_ACTIVE_NOW,
            |_| { FetchOutcome::Outage }
        )
        .plan,
        Plan::Community
    );
    clear_denial(&fixture.paths.cache, VECTOR_ACCOUNT);
    std::fs::remove_file(account_status_path(&fixture.paths, VECTOR_ACCOUNT)).unwrap();
    assert_eq!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW).verification_status,
        "denied"
    );
    assert!(
        resolve_at(&fixture.paths, VECTOR_ACTIVE_NOW)
            .current_allows_at("compression", VECTOR_ACTIVE_NOW)
    );
}
