//! End-to-end tests for cross-agent delivery via the daemon's OCLA wire API.
//!
//! Simulates two agents (separate Cursor tabs) sharing file reads through the
//! daemon's `/ocla/v1/delivery/{record,check,stats}` endpoints. This validates
//! the complete cross-process pipeline without requiring actual Unix sockets.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

fn ocla_app() -> axum::Router {
    lean_ctx::core::ocla::wire_api::ocla_router()
}

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn post_record(app: &axum::Router, agent: &str, path: &str, hash: [u8; 12], mtime: u64) {
    let entry = json!({
        "blake3": hash,
        "path": path,
        "line_count": 150,
        "agent_id": agent,
        "conversation_id": format!("conv-{agent}"),
        "mtime": mtime,
        "token_count": 500,
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/record", &entry))
        .await
        .expect("record response");
    assert!(
        resp.status().is_success(),
        "record returned {}",
        resp.status()
    );
}

async fn post_check(app: &axum::Router, path: &str, hash: [u8; 12], mtime: u64) -> Value {
    let body = json!({"blake3": hash, "mtime": mtime, "path": path});
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/check", &body))
        .await
        .expect("check response");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

async fn get_stats(app: &axum::Router) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ocla/v1/delivery/stats")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("stats response");
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn two_agents_share_reads_via_delivery_endpoints() {
    let app = ocla_app();
    let hash: [u8; 12] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
    let mtime = 1719000000;

    let before = post_check(&app, "src/config.rs", hash, mtime).await;
    assert_eq!(before["hit"], false, "no prior record → miss");

    post_record(&app, "cursor-tab-1", "src/config.rs", hash, mtime).await;

    let hit = post_check(&app, "src/config.rs", hash, mtime).await;
    assert_eq!(hit["hit"], true, "agent-2 sees agent-1's read");
    assert_eq!(hit["agent_id"], "cursor-tab-1");
    assert_eq!(hit["path"], "src/config.rs");
    assert_eq!(hit["line_count"], 150);
}

#[tokio::test]
async fn stale_mtime_still_hits_same_hash() {
    let app = ocla_app();
    let hash: [u8; 12] = [11, 21, 31, 41, 51, 61, 71, 81, 91, 101, 111, 121];

    post_record(&app, "agent-a", "src/lib.rs", hash, 1000).await;

    // blake3 is the content identity; mtime changes (checkout/rebase) don't
    // invalidate the cache entry.
    let stale = post_check(&app, "src/lib.rs", hash, 2000).await;
    assert_eq!(stale["hit"], true, "same blake3 → hit regardless of mtime");
}

#[tokio::test]
async fn stats_track_cross_agent_activity() {
    let app = ocla_app();
    let hash_a: [u8; 12] = [12, 22, 32, 42, 52, 62, 72, 82, 92, 102, 112, 122];
    let hash_b: [u8; 12] = [13, 23, 33, 43, 53, 63, 73, 83, 93, 103, 113, 123];

    let stats_before = get_stats(&app).await;
    let entries_before = stats_before["total_entries"].as_u64().unwrap();

    post_record(&app, "agent-1", "src/a.rs", hash_a, 500).await;
    post_record(&app, "agent-2", "src/b.rs", hash_b, 600).await;

    let stats_after = get_stats(&app).await;
    let entries_after = stats_after["total_entries"].as_u64().unwrap();
    assert!(
        entries_after >= entries_before + 2,
        "stats must reflect new records"
    );
}

#[tokio::test]
async fn lookup_hits_do_not_claim_unserved_token_savings() {
    let app = ocla_app();
    let hash: [u8; 12] = [14, 24, 34, 44, 54, 64, 74, 84, 94, 104, 114, 124];

    post_record(&app, "agent-writer", "src/main.rs", hash, 999).await;

    let stats_before = get_stats(&app).await;
    let saved_before = stats_before["tokens_saved"].as_u64().unwrap();

    let stubs_before = stats_before["stubs_served"].as_u64().unwrap();
    assert_eq!(
        post_check(&app, "src/main.rs", hash, 999).await["hit"],
        true
    );
    assert_eq!(
        post_check(&app, "src/main.rs", hash, 999).await["hit"],
        true
    );

    let stats_after = get_stats(&app).await;
    let saved_after = stats_after["tokens_saved"].as_u64().unwrap();
    assert_eq!(
        saved_after, saved_before,
        "lookup alone must not claim savings before an actual response is served"
    );
    let stubs = stats_after["stubs_served"].as_u64().unwrap();
    assert_eq!(stubs, stubs_before, "lookup is not a served stub");
}

#[tokio::test]
async fn multiple_agents_multiple_files_isolation() {
    let app = ocla_app();
    let hash_1: [u8; 12] = [15, 25, 35, 45, 55, 65, 75, 85, 95, 105, 115, 125];
    let hash_2: [u8; 12] = [16, 26, 36, 46, 56, 66, 76, 86, 96, 106, 116, 126];

    post_record(&app, "tab-A", "src/models.rs", hash_1, 3000).await;
    post_record(&app, "tab-B", "src/routes.rs", hash_2, 4000).await;

    let check_1 = post_check(&app, "src/models.rs", hash_1, 3000).await;
    assert_eq!(check_1["hit"], true);
    assert_eq!(check_1["agent_id"], "tab-A");

    let check_2 = post_check(&app, "src/routes.rs", hash_2, 4000).await;
    assert_eq!(check_2["hit"], true);
    assert_eq!(check_2["agent_id"], "tab-B");

    // mtime is no longer a cache discriminator — same hash = hit
    let check_different_mtime = post_check(&app, "src/models.rs", hash_1, 9999).await;
    assert_eq!(check_different_mtime["hit"], true);
    let wrong_path = post_check(&app, "src/routes.rs", hash_1, 3000).await;
    assert_eq!(
        wrong_path["hit"], false,
        "hash identity cannot bypass path isolation"
    );
}

#[tokio::test]
async fn relay_content_recorded_and_served_via_wire() {
    let app = ocla_app();
    let hash: [u8; 12] = [17, 27, 37, 47, 57, 67, 77, 87, 97, 107, 117, 127];
    let mtime = 5000;
    let relay_text = "pub struct Config {\n    port: u16,\n    host: String,\n}";

    let entry = json!({
        "blake3": hash,
        "path": "src/config.rs",
        "line_count": 4,
        "agent_id": "local-1001",
        "conversation_id": "conv-local-1001",
        "mtime": mtime,
        "token_count": 800,
        "relay_content": relay_text,
        "relay_mode": "map:v2",
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/record", &entry))
        .await
        .expect("record response");
    assert!(resp.status().is_success());

    let hit = post_check(&app, "src/config.rs", hash, mtime).await;
    assert_eq!(hit["hit"], true);
    assert_eq!(
        hit["relay_content"].as_str().unwrap(),
        relay_text,
        "relay_content must be served back to requesting agent"
    );
    assert_eq!(hit["relay_mode"], "map:v2");
}

#[tokio::test]
async fn relay_lookup_does_not_claim_served_savings() {
    let app = ocla_app();
    let hash: [u8; 12] = [18, 28, 38, 48, 58, 68, 78, 88, 98, 108, 118, 128];

    let entry = json!({
        "blake3": hash,
        "path": "src/relay_stats.rs",
        "line_count": 50,
        "agent_id": "local-2002",
        "conversation_id": "conv-local-2002",
        "mtime": 6000,
        "token_count": 1200,
        "relay_content": "fn main() {}",
        "relay_mode": "signatures:v2",
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/record", &entry))
        .await
        .expect("record");
    assert!(resp.status().is_success());

    let stats_before = get_stats(&app).await;
    let relay_before = stats_before["relay_served"].as_u64().unwrap();
    let saved_before = stats_before["relay_tokens_saved"].as_u64().unwrap();

    let hit = post_check(&app, "src/relay_stats.rs", hash, 6000).await;
    assert_eq!(hit["hit"], true);
    assert_eq!(hit["relay_content"], "fn main() {}");

    let stats_after = get_stats(&app).await;
    let relay_after = stats_after["relay_served"].as_u64().unwrap();
    assert_eq!(
        relay_after, relay_before,
        "lookup is not an actual served relay response"
    );
    assert_eq!(
        stats_after["relay_tokens_saved"].as_u64().unwrap(),
        saved_before,
        "lookup cannot invent relay savings"
    );
}

#[tokio::test]
async fn pid_differentiation_enables_cross_agent_hits() {
    let app = ocla_app();
    let hash: [u8; 12] = [19, 29, 39, 49, 59, 69, 79, 89, 99, 109, 119, 129];

    let entry = json!({
        "blake3": hash,
        "path": "src/pid_test.rs",
        "line_count": 30,
        "agent_id": "local-9876",
        "conversation_id": "conv-local-9876",
        "mtime": 7000,
        "token_count": 600,
        "relay_content": "mod pid_test;",
        "relay_mode": "map:v2",
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/record", &entry))
        .await
        .expect("record");
    assert!(resp.status().is_success());

    // Different PID agent checks → must get relay hit
    let check_body = json!({
        "blake3": hash,
        "mtime": 7000,
        "path": "src/pid_test.rs",
        "requester_agent_id": "local-5432",
        "requester_conversation_id": "conv-local-5432",
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/check", &check_body))
        .await
        .expect("check response");
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(result["hit"], true, "different PID must hit");
    assert_eq!(result["relay_content"], "mod pid_test;");
    assert_eq!(result["agent_id"], "local-9876");

    // Same PID agent checks → must NOT hit (self-read)
    let self_body = json!({
        "blake3": hash,
        "mtime": 7000,
        "path": "src/pid_test.rs",
        "requester_agent_id": "local-9876",
        "requester_conversation_id": "conv-local-9876",
    });
    let resp = app
        .clone()
        .oneshot(json_request("POST", "/ocla/v1/delivery/check", &self_body))
        .await
        .expect("self-check response");
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["hit"], false, "same agent must NOT self-hit");
}
