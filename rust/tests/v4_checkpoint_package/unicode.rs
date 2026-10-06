// SPDX-License-Identifier: Apache-2.0
//! V3 signs exact Unicode bytes through the real host, never a package-key grant.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, SigningKey};
use lean_ctx_protocol::{
    CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN, CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN,
    ContextCheckpointV2, ContextCheckpointV3,
};

pub(super) fn exercise(
    root: &Path,
    prior: &ContextCheckpointV2,
    receipt: &Value,
    settings: &Value,
) {
    let old_bytes = prior.canonical_bytes().unwrap();
    let mut value = serde_json::to_value(prior).unwrap();
    value["schema_version"] = json!(3);
    value["live_state"]["schema_version"] = json!(2);
    value["live_state"]["task"]["title"] = json!("Fortsetzung für Zürich — 東京");
    value["live_state"]["progress"]["summary"] = json!("Première étape vérifiée");
    value["live_state"]["findings"] = json!(["Überprüfung abgeschlossen", "結果を保持"]);
    value["live_state"]["next_steps"] = json!(["État exact conserver", "المتابعة"]);
    value["live_state"]["handoff_summary"] = json!("Grüße · résumé · 文脈");
    value["live_state"]["decisions"] = json!([{
        "decision_id":"unicode-decision", "status":"accepted", "evidence_refs":[],
        "statement":"Données conservées", "rationale":"Keine verlustbehaftete Änderung"
    }]);
    let checkpoint: ContextCheckpointV3 = serde_json::from_value(value.clone()).unwrap();
    assert!(
        ContextCheckpointV2::from_canonical_bytes(&checkpoint.canonical_bytes().unwrap()).is_err()
    );
    let request = json!({"schema_version":3,
        "checkpoint_json":checkpoint.canonical_json().unwrap(), "receipt_digests":[receipt]});
    let exported = success(&host_call(
        root,
        &["context-checkpoint", "--json"],
        &request,
        settings,
    ));
    assert_eq!(
        exported["schema_version"],
        "leanctx.host-checkpoint-result/v2"
    );
    assert_eq!(
        exported["artifact"]["schema_version"],
        "leanctx.host-checkpoint/v2"
    );
    assert_eq!(exported["artifact"]["checkpoint"], value);
    assert_eq!(
        exported["checkpoint_digest"],
        json!(checkpoint.digest().unwrap())
    );
    assert_eq!(exported["session_adopted"], false);
    let artifact_hash = exported["artifact_digest"].as_str().unwrap();
    let artifact_path = root.join("data/execution/checkpoints").join(format!(
        "{}.json",
        artifact_hash.strip_prefix("sha256:").unwrap()
    ));
    let artifact_bytes = std::fs::read(&artifact_path).unwrap();
    assert_eq!(
        artifact_bytes,
        lean_ctx::core::canonical::canonical_serialize(&exported["artifact"])
    );
    {
        use sha2::{Digest as _, Sha256};
        assert_eq!(
            artifact_hash,
            format!("sha256:{}", hex_bytes(&Sha256::digest(&artifact_bytes)))
        );
    }
    let artifact_modified = artifact_path.metadata().unwrap().modified().unwrap();
    let signature = Signature::from_slice(
        &STANDARD
            .decode(exported["artifact"]["signature"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let key = SigningKey::from_bytes(&[71; 32]);
    let mut signed = CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN.to_vec();
    signed.extend(checkpoint.canonical_bytes().unwrap());
    key.verifying_key()
        .verify_strict(&signed, &signature)
        .unwrap();
    let mut wrong_domain = CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.to_vec();
    wrong_domain.extend(checkpoint.canonical_bytes().unwrap());
    assert!(
        key.verifying_key()
            .verify_strict(&wrong_domain, &signature)
            .is_err()
    );
    assert_eq!(
        success(&host_call(
            root,
            &["context-checkpoint", "--json"],
            &request,
            settings
        )),
        exported
    );
    assert_eq!(prior.canonical_bytes().unwrap(), old_bytes);
    assert_eq!(
        artifact_path.metadata().unwrap().modified().unwrap(),
        artifact_modified
    );

    let artifact_dir = root.join("data/execution/checkpoints");
    let before = std::fs::read_dir(&artifact_dir).unwrap().count();
    for case in 0..7 {
        let mut attempt = request.clone();
        let mut grant = settings.clone();
        let mut candidate = value.clone();
        match case {
            0 => attempt["schema_version"] = json!(2),
            1 => grant["allow_checkpoint_signing"] = json!(false),
            2 => candidate["lineage"]["tenant_id"] = json!("foreign-tenant"),
            3 => {
                candidate["lineage"]["artifact_lineage"]["task_ref"] =
                    json!(format!("sha256:{}", "0".repeat(64)));
            }
            4 => candidate["live_state"]["task"]["title"] = json!("text\u{202e}hidden"),
            5 => candidate["live_state"]["findings"] = json!(["/private/unicode-source"]),
            _ => {
                attempt["checkpoint_json"] =
                    json!(format!("{} ", checkpoint.canonical_json().unwrap()));
            }
        }
        if matches!(case, 2..=5) {
            attempt["checkpoint_json"] = json!(
                String::from_utf8(lean_ctx::core::canonical::canonical_serialize(&candidate))
                    .unwrap()
            );
        }
        let denied = host_call(root, &["context-checkpoint", "--json"], &attempt, &grant);
        assert!(
            !denied.status.success(),
            "Unicode host accepted denial {case}"
        );
        assert!(denied.stdout.is_empty());
        assert_eq!(std::fs::read_dir(&artifact_dir).unwrap().count(), before);
    }

    let portable = json!({"schema_version":"leanctx.ctxpkg-checkpoint/v4",
        "checkpoint":value, "non_portable_fields":[]});
    success(&seal(root, &portable));
    let inspected = success(&inspect(root));
    assert_eq!(inspected["checkpoint"], portable);
    assert_eq!(inspected["package"]["signature_state"], "signed_valid");
    let path = root.join("state.ctxpkg");
    let package_bytes = std::fs::read(&path).unwrap();
    for case in 0..3 {
        let mut denied = portable.clone();
        match case {
            0 => denied["schema_version"] = json!("leanctx.ctxpkg-checkpoint/v3"),
            1 => denied["checkpoint"] = serde_json::to_value(prior).unwrap(),
            _ => denied["checkpoint"]["live_state"]["schema_version"] = json!(1),
        }
        assert!(!seal(root, &denied).status.success());
        assert_eq!(std::fs::read(&path).unwrap(), package_bytes);
    }

    let mut receiving = settings.clone();
    receiving["checkpoint_resume"] = json!({
        "project_root":std::fs::canonicalize(root).unwrap(),
        "project_id":"project-carrier", "tenant_id":"tenant-carrier",
        "workspace_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    });
    let resumed = success(&host_call(
        root,
        &["context-checkpoint-resume", "--json"],
        &json!({"schema_version":1,"artifact_digest":exported["artifact_digest"]}),
        &receiving,
    ));
    assert_eq!(
        resumed["schema_version"],
        "leanctx.host-checkpoint-resume-result/v2"
    );
    assert_eq!(resumed["canonical_session_adopted"], true);
    assert_eq!(resumed["checkpoint_digest"], exported["checkpoint_digest"]);
    let id = resumed["session_id"].as_str().unwrap();
    let session_path = root.join("data/sessions").join(format!("{id}.json"));
    let session_bytes = std::fs::read(&session_path).unwrap();
    let session: Value = serde_json::from_slice(&session_bytes).unwrap();
    assert_eq!(session["storage_schema"], "leanctx.session-checkpoint/v1");
    assert_eq!(session["canonical"]["checkpoint"]["schema_version"], 3);
    assert_eq!(
        session["view"]["task"]["description"],
        "Fortsetzung für Zürich — 東京"
    );
    assert_eq!(
        session["view"]["findings"][0]["summary"],
        "Überprüfung abgeschlossen"
    );
    assert_eq!(session["view"]["findings"][1]["summary"], "結果を保持");
    assert_eq!(session["view"]["next_steps"][0], "État exact conserver");
    assert_eq!(session["view"]["next_steps"][1], "المتابعة");
    assert_eq!(
        session["view"]["decisions"][0]["summary"],
        "Données conservées"
    );
    assert_eq!(
        session["view"]["progress"][0]["detail"],
        "Grüße · résumé · 文脈"
    );
    let loaded = lean_ctx::core::session::SessionState::from_storage_json(
        std::str::from_utf8(&session_bytes).unwrap(),
    )
    .unwrap();
    assert_eq!(
        loaded.task.as_ref().unwrap().description,
        "Fortsetzung für Zürich — 東京"
    );
    assert_eq!(loaded.findings[1].summary, "結果を保持");
    assert_eq!(loaded.decisions[0].summary, "Données conservées");

    let sessions = root.join("data/sessions");
    let before = std::fs::read_dir(&sessions).unwrap().count();
    for (field, foreign) in [
        ("tenant_id", "foreign-tenant"),
        ("project_id", "foreign-project"),
        ("workspace_id", "ffffffff-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
    ] {
        let mut denied_scope = receiving.clone();
        denied_scope["checkpoint_resume"][field] = json!(foreign);
        let denied = host_call(
            root,
            &["context-checkpoint-resume", "--json"],
            &json!({"schema_version":1,"artifact_digest":exported["artifact_digest"]}),
            &denied_scope,
        );
        assert!(
            !denied.status.success(),
            "foreign {field} resumed Unicode state"
        );
        assert!(denied.stdout.is_empty());
        assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), before);
        assert_eq!(std::fs::read(&session_path).unwrap(), session_bytes);
    }

    for (action, text) in [
        ("task", "Zürich weiterführen — 次の段階"),
        // The CLI reserves an em dash for `file — summary`; use plain finding text.
        ("finding", "النص محفوظ · weiterer Befund"),
        ("decision", "Préserver la continuité"),
    ] {
        let changed = isolated_cli(root)
            .args(["session", action, text])
            .output()
            .unwrap();
        assert!(
            changed.status.success(),
            "{}",
            String::from_utf8_lossy(&changed.stderr)
        );
    }
    let continued_bytes = std::fs::read(&session_path).unwrap();
    let continued: Value = serde_json::from_slice(&continued_bytes).unwrap();
    let current = &continued["canonical"]["checkpoint"];
    assert_eq!(current["schema_version"], 3);
    assert_eq!(
        current["live_state"]["task"]["title"],
        "Zürich weiterführen — 次の段階"
    );
    assert_eq!(current["lineage"], value["lineage"]);
    assert!(
        current["live_state"]["findings"]
            .as_array()
            .unwrap()
            .contains(&json!("النص محفوظ · weiterer Befund"))
    );
    assert!(
        current["live_state"]["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|decision| decision["statement"] == "Préserver la continuité")
    );
    let reloaded = lean_ctx::core::session::SessionState::from_storage_json(
        std::str::from_utf8(&continued_bytes).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reloaded.task.unwrap().description,
        "Zürich weiterführen — 次の段階"
    );

    let continued_package = root.join("unicode-continued.ctxpkg");
    success(
        &cli(root)
            .arg("checkpoint-seal")
            .arg(format!("--session={id}"))
            .arg(format!("--output={}", continued_package.display()))
            .arg("--name=unicode-continuation")
            .env("LEAN_CTX_ROLE", "admin")
            .output()
            .unwrap(),
    );
    let inspected = success(
        &cli(root)
            .arg("checkpoint-inspect")
            .arg(&continued_package)
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspected["checkpoint"]["schema_version"],
        "leanctx.ctxpkg-checkpoint/v4"
    );
    assert_eq!(inspected["checkpoint"]["checkpoint"], *current);
    assert_eq!(inspected["package"]["signature_state"], "signed_valid");
    assert_eq!(std::fs::read(&session_path).unwrap(), continued_bytes);
    assert_eq!(std::fs::read(&artifact_path).unwrap(), artifact_bytes);
}
