//! Unit tests for the proxy config (split from `proxy.rs` — LOC budget).

use super::*;

#[test]
fn loopback_http_is_always_allowed() {
    assert_eq!(
        validate_upstream_url("http://127.0.0.1:4444", false, false).unwrap(),
        "http://127.0.0.1:4444"
    );
    assert_eq!(
        validate_upstream_url("http://localhost:2455/", false, false).unwrap(),
        "http://localhost:2455"
    );
}

#[test]
fn https_allowlisted_host_is_allowed() {
    assert_eq!(
        validate_upstream_url("https://api.openai.com", false, false).unwrap(),
        "https://api.openai.com"
    );
}

#[test]
fn non_loopback_http_is_rejected_without_optin() {
    let err = validate_upstream_url("http://host.docker.internal:2455", false, false).unwrap_err();
    // The hint must point at the flag that actually lifts the scheme check
    // (#440). The old message pointed at LEAN_CTX_ALLOW_CUSTOM_UPSTREAM,
    // which never bypassed the HTTPS requirement.
    assert!(
        err.contains("LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM"),
        "hint must name the working opt-in, got: {err}"
    );
}

#[test]
fn non_loopback_http_is_allowed_with_optin() {
    assert_eq!(
        validate_upstream_url("http://host.docker.internal:2455", true, false).unwrap(),
        "http://host.docker.internal:2455"
    );
}

#[test]
fn unknown_scheme_is_rejected() {
    assert!(validate_upstream_url("ftp://example.com", true, true).is_err());
}

#[test]
fn https_custom_host_is_rejected_without_optin() {
    // #590: a custom HTTPS host (e.g. a corporate gateway) is blocked unless
    // the operator opts in. The hint must name BOTH the env var and the
    // config flag — only the config flag reaches the managed proxy.
    let err = validate_upstream_url("https://gw.corp.example/anthropic", false, false).unwrap_err();
    assert!(
        err.contains("LEAN_CTX_ALLOW_CUSTOM_UPSTREAM") && err.contains("allow_custom_upstream"),
        "hint must name both opt-ins, got: {err}"
    );
}

#[test]
fn https_custom_host_is_allowed_with_optin() {
    // The opt-in (env or `[proxy] allow_custom_upstream`) lifts the allowlist.
    assert_eq!(
        validate_upstream_url("https://gw.corp.example/anthropic", false, true).unwrap(),
        "https://gw.corp.example/anthropic"
    );
}

#[test]
fn config_flag_enables_custom_upstream_optin() {
    // #590: mirrors `config_flag_enables_insecure_http_optin`. `Some(true)`
    // resolves to true regardless of the environment, so no env mutation.
    let cfg = ProxyConfig {
        allow_custom_upstream: Some(true),
        ..Default::default()
    };
    assert!(cfg.allows_custom_upstream());
}

#[test]
fn has_custom_host_upstream_detects_only_custom_https() {
    // A custom HTTPS host counts; an allowlisted host, a loopback URL, and an
    // unset upstream do not (the http case is the insecure-http opt-in's job).
    assert!(
        ProxyConfig {
            anthropic_upstream: Some("https://gw.corp.example/anthropic".into()),
            ..Default::default()
        }
        .has_custom_host_upstream()
    );
    assert!(
        !ProxyConfig {
            openai_upstream: Some("https://api.openai.com".into()),
            anthropic_upstream: Some("http://127.0.0.1:4444".into()),
            ..Default::default()
        }
        .has_custom_host_upstream()
    );
    assert!(!ProxyConfig::default().has_custom_host_upstream());
}

#[test]
fn cold_prefix_repack_is_opt_in_and_config_enables() {
    // #480: off by default (a wrong cold guess re-bills reads as writes ~12x),
    // enabled via config. Isolate from a developer shell that may export the
    // env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
    assert!(
        !ProxyConfig::default().repacks_cold_prefix(),
        "cold-prefix repack must be opt-in (off by default)"
    );
    let cfg = ProxyConfig {
        cold_prefix_repack: Some(true),
        ..Default::default()
    };
    assert!(cfg.repacks_cold_prefix());
}

#[test]
fn ccr_inband_is_opt_in_and_config_enables() {
    // #493: off by default (the splice mutates provider-visible content for
    // the expand turn), enabled via config. Isolate from a developer shell
    // that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CCR_INBAND");
    assert!(
        !ProxyConfig::default().ccr_inband_enabled(),
        "in-band CCR must be opt-in (off by default)"
    );
    let cfg = ProxyConfig {
        ccr_inband: Some(true),
        ..Default::default()
    };
    assert!(cfg.ccr_inband_enabled());
}

#[test]
fn cache_breakpoint_is_opt_in_and_config_enables() {
    // #939: off by default (it reshapes the provider-visible system field),
    // enabled via config. Isolate from a developer shell that may export the
    // env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    assert!(
        !ProxyConfig::default().cache_breakpoint_enabled(),
        "cache-breakpoint injection must be opt-in (off by default)"
    );
    let cfg = ProxyConfig {
        cache_breakpoint: Some(true),
        ..Default::default()
    };
    assert!(cfg.cache_breakpoint_enabled());
}

#[test]
fn cache_aligner_defaults_on_and_config_disables() {
    // #986 premium defaults: the volatile-field scan is measurement-only and
    // strictly cache-safe, so it ships on by default; `false` opts out.
    // Isolate from a developer shell that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGNER");
    assert!(
        ProxyConfig::default().cache_aligner_enabled(),
        "cache-aligner telemetry must be on by default (measurement-only, safe)"
    );
    let cfg = ProxyConfig {
        cache_aligner: Some(false),
        ..Default::default()
    };
    assert!(!cfg.cache_aligner_enabled(), "explicit false opts out");
}

#[test]
fn cache_aligner_legacy_opt_in_still_enables() {
    // An explicit `true` (a pre-#986 config) keeps working unchanged. Isolate
    // from a developer shell that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGNER");
    let cfg = ProxyConfig {
        cache_aligner: Some(true),
        ..Default::default()
    };
    assert!(cfg.cache_aligner_enabled());
}

#[test]
fn cache_align_relocate_is_opt_in_and_config_enables() {
    // #974: off by default (it reshapes the provider-visible system field by
    // relocating volatile values to the tail). Isolate from a developer shell
    // that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGN_RELOCATE");
    assert!(
        !ProxyConfig::default().cache_align_relocate_enabled(),
        "active cache-aligner relocate must be opt-in (off by default)"
    );
    let cfg = ProxyConfig {
        cache_align_relocate: Some(true),
        ..Default::default()
    };
    assert!(cfg.cache_align_relocate_enabled());
}

#[test]
fn cache_policy_defaults_on_and_can_be_disabled() {
    // #986 premium defaults: telemetry + a more-conservative repack gate are
    // both strictly safe, so cache-economics ships on by default and is
    // opt-out via config `false` or `LEAN_CTX_PROXY_CACHE_POLICY=off`. Isolate
    // from a developer shell that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_POLICY");
    assert!(
        ProxyConfig::default().cache_policy_enabled(),
        "cache-economics must be on by default (measurement + safe gate)"
    );
    let cfg = ProxyConfig {
        cache_policy: Some(false),
        ..Default::default()
    };
    assert!(!cfg.cache_policy_enabled(), "explicit false opts out");

    // An explicit env `off` wins even over a config `true`.
    crate::test_env::set_var("LEAN_CTX_PROXY_CACHE_POLICY", "off");
    let on = ProxyConfig {
        cache_policy: Some(true),
        ..Default::default()
    };
    assert!(!on.cache_policy_enabled(), "env off overrides config true");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_POLICY");
}

#[test]
fn effort_defaults_off_and_config_sets_it() {
    // #834: cache-safe effort control is opt-in. Isolate from a developer
    // shell that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
    assert_eq!(
        ProxyConfig::default().resolved_effort(),
        None,
        "effort control must be opt-in (off by default)"
    );
    let cfg = ProxyConfig {
        effort: Some("low".into()),
        ..Default::default()
    };
    assert_eq!(
        cfg.resolved_effort(),
        Some(crate::core::config::Effort::Low)
    );
    // An unknown configured value resolves to off — never a silent default.
    let typo = ProxyConfig {
        effort: Some("lowish".into()),
        ..Default::default()
    };
    assert_eq!(typo.resolved_effort(), None);
}

#[test]
fn effort_env_overrides_and_off_disables() {
    use crate::core::config::Effort;
    let _lock = crate::core::data_dir::test_env_lock();
    let cfg = ProxyConfig {
        effort: Some("high".into()),
        ..Default::default()
    };
    // A valid env level wins over config.
    crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "minimal");
    assert_eq!(cfg.resolved_effort(), Some(Effort::Minimal));
    // `off` explicitly disables even a configured level.
    crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "off");
    assert_eq!(cfg.resolved_effort(), None);
    // A blank/garbage env value is ignored → falls back to config.
    crate::test_env::set_var("LEAN_CTX_PROXY_EFFORT", "   ");
    assert_eq!(cfg.resolved_effort(), Some(Effort::High));
    crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
}

#[test]
fn prose_ranker_defaults_to_auto_and_config_sets_it() {
    // #895: premium extractive path is the default; `truncate`/`off` selects
    // the legacy squeeze; a typo can never silently disable the premium path.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_PROSE_RANKER");
    assert_eq!(
        ProxyConfig::default().resolved_prose_ranker(),
        ProseRanker::Auto
    );
    let truncate = ProxyConfig {
        prose_ranker: Some("truncate".into()),
        ..Default::default()
    };
    assert_eq!(truncate.resolved_prose_ranker(), ProseRanker::Truncate);
    let off = ProxyConfig {
        prose_ranker: Some("off".into()),
        ..Default::default()
    };
    assert_eq!(off.resolved_prose_ranker(), ProseRanker::Truncate);
    let extractive = ProxyConfig {
        prose_ranker: Some("extractive".into()),
        ..Default::default()
    };
    assert_eq!(extractive.resolved_prose_ranker(), ProseRanker::Extractive);
    let typo = ProxyConfig {
        prose_ranker: Some("extractiveish".into()),
        ..Default::default()
    };
    assert_eq!(
        typo.resolved_prose_ranker(),
        ProseRanker::Auto,
        "unknown value must resolve to Auto, never silently off"
    );
}

#[test]
fn output_holdout_defaults_off_and_clamps() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_OUTPUT_HOLDOUT");
    assert_eq!(ProxyConfig::default().output_holdout_fraction(), 0.0);
    let cfg = ProxyConfig {
        output_holdout: Some(0.2),
        ..Default::default()
    };
    assert!((cfg.output_holdout_fraction() - 0.2).abs() < f64::EPSILON);
    let over = ProxyConfig {
        output_holdout: Some(5.0),
        ..Default::default()
    };
    assert_eq!(over.output_holdout_fraction(), 1.0, "clamped into [0,1]");
}

#[test]
fn verbosity_steer_defaults_off_and_env_overrides() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
    assert!(!ProxyConfig::default().verbosity_steer_enabled());
    let cfg = ProxyConfig {
        verbosity_steer: Some(true),
        ..Default::default()
    };
    assert!(cfg.verbosity_steer_enabled());
    crate::test_env::set_var("LEAN_CTX_PROXY_VERBOSITY_STEER", "on");
    assert!(ProxyConfig::default().verbosity_steer_enabled());
    crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
}

#[test]
fn codex_chatgpt_proxy_flag_reads_config_and_env() {
    // Isolate from a developer shell that may export the env override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_CODEX_CHATGPT_PROXY");
    assert!(
        !ProxyConfig::default().codex_chatgpt_proxy_enabled(),
        "Codex ChatGPT proxy opt-in defaults off"
    );
    let cfg = ProxyConfig {
        codex_chatgpt_proxy: Some(true),
        ..Default::default()
    };
    assert!(cfg.codex_chatgpt_proxy_enabled());
    // An explicit env value wins even over an unset/false config.
    crate::test_env::set_var("LEAN_CTX_CODEX_CHATGPT_PROXY", "1");
    assert!(ProxyConfig::default().codex_chatgpt_proxy_enabled());
    crate::test_env::remove_var("LEAN_CTX_CODEX_CHATGPT_PROXY");
}

#[test]
fn prose_ranker_env_overrides_config() {
    let _lock = crate::core::data_dir::test_env_lock();
    let cfg = ProxyConfig {
        prose_ranker: Some("auto".into()),
        ..Default::default()
    };
    crate::test_env::set_var("LEAN_CTX_PROXY_PROSE_RANKER", "truncate");
    assert_eq!(cfg.resolved_prose_ranker(), ProseRanker::Truncate);
    crate::test_env::remove_var("LEAN_CTX_PROXY_PROSE_RANKER");
}

#[test]
fn config_flag_enables_insecure_http_optin() {
    // `Some(true)` resolves to `true` regardless of the environment, so this
    // assertion is robust without mutating process-global env vars.
    let cfg = ProxyConfig {
        allow_insecure_http_upstream: Some(true),
        ..Default::default()
    };
    assert!(cfg.allows_insecure_http_upstream());
}

/// `resolve_all_disk` ignores `LEAN_CTX_*_UPSTREAM` env by construction, so
/// these assertions are env-independent (no lock needed). Loopback HTTP is an
/// always-valid custom upstream (no allowlist / opt-in required).
#[test]
fn resolve_all_disk_uses_config_then_default() {
    let cfg = ProxyConfig {
        openai_upstream: Some("http://127.0.0.1:19101".into()),
        ..Default::default()
    };
    let up = cfg.resolve_all_disk();
    assert_eq!(up.openai, "http://127.0.0.1:19101");
    assert_eq!(up.anthropic, "https://api.anthropic.com");
    assert_eq!(up.chatgpt, "https://chatgpt.com");
    assert_eq!(up.gemini, "https://generativelanguage.googleapis.com");
}

#[test]
fn resolve_all_disk_honors_custom_upstream_via_config_flag() {
    // #590: `resolve_all_disk` is the env-independent view — exactly what the
    // managed (service-spawned) proxy serves, since it never sees the shell's
    // LEAN_CTX_ALLOW_CUSTOM_UPSTREAM. With the config opt-in, a custom HTTPS
    // host resolves; without it, it falls back to the provider default. This
    // is the regression guard for the reported bug.
    let custom = ProxyConfig {
        anthropic_upstream: Some("https://gw.corp.example/anthropic".into()),
        allow_custom_upstream: Some(true),
        ..Default::default()
    };
    assert_eq!(
        custom.resolve_all_disk().anthropic,
        "https://gw.corp.example/anthropic",
        "config flag must let the managed proxy honor the custom upstream"
    );

    let blocked = ProxyConfig {
        anthropic_upstream: Some("https://gw.corp.example/anthropic".into()),
        ..Default::default()
    };
    // Isolate from a developer shell that may export the env opt-in.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_ALLOW_CUSTOM_UPSTREAM");
    assert_eq!(
        blocked.resolve_all_disk().anthropic,
        "https://api.anthropic.com",
        "without the opt-in the custom host is rejected → provider default"
    );
}

#[test]
fn resolve_all_disk_normalizes_trailing_slash() {
    let cfg = ProxyConfig {
        openai_upstream: Some("http://127.0.0.1:19101/".into()),
        ..Default::default()
    };
    assert_eq!(cfg.resolve_all_disk().openai, "http://127.0.0.1:19101");
}

#[test]
fn refresh_keeps_last_good_on_invalid_config() {
    // `refresh_upstreams` is env-aware; isolate from a developer's shell that
    // may export LEAN_CTX_OPENAI_UPSTREAM (e.g. while reproducing #449).
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");

    // A typo in config.toml must never reroute a live proxy to the default.
    let last = Upstreams {
        anthropic: "https://api.anthropic.com".into(),
        openai: "http://127.0.0.1:19101".into(),
        chatgpt: "https://chatgpt.com".into(),
        gemini: "https://generativelanguage.googleapis.com".into(),
        providers: Vec::new(),
    };
    let cfg = ProxyConfig {
        openai_upstream: Some("not-a-valid-url".into()),
        ..Default::default()
    };
    assert_eq!(
        cfg.refresh_upstreams(&last).openai,
        "http://127.0.0.1:19101",
        "invalid upstream → keep last good, never silently fall to default"
    );
}

#[test]
fn refresh_adopts_valid_config_change() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_OPENAI_UPSTREAM");

    let last = Upstreams {
        anthropic: "https://api.anthropic.com".into(),
        openai: "http://127.0.0.1:19101".into(),
        chatgpt: "https://chatgpt.com".into(),
        gemini: "https://generativelanguage.googleapis.com".into(),
        providers: Vec::new(),
    };
    let cfg = ProxyConfig {
        openai_upstream: Some("http://127.0.0.1:19102".into()),
        ..Default::default()
    };
    assert_eq!(
        cfg.refresh_upstreams(&last).openai,
        "http://127.0.0.1:19102"
    );
}

#[test]
fn diagnose_drift_env_set_but_proxy_serves_other() {
    // The exact #449 / Codex case: env exported in the shell, but the
    // MCP-spawned proxy serves config.toml → the env never reached it.
    assert_eq!(
        diagnose_drift(
            Some("http://127.0.0.1:2455"),
            "https://api.openai.com",
            "https://api.openai.com"
        ),
        Some(UpstreamDrift::EnvNotApplied)
    );
}

#[test]
fn diagnose_drift_env_consistent_is_in_sync() {
    // Proxy was started with the env value and serves it → not drift.
    assert_eq!(
        diagnose_drift(
            Some("http://127.0.0.1:2455"),
            "https://api.openai.com",
            "http://127.0.0.1:2455"
        ),
        None
    );
}

#[test]
fn diagnose_drift_config_changed_needs_restart() {
    assert_eq!(
        diagnose_drift(None, "http://127.0.0.1:2455", "https://api.openai.com"),
        Some(UpstreamDrift::ConfigNotApplied)
    );
}

#[test]
fn diagnose_drift_in_sync() {
    assert_eq!(
        diagnose_drift(None, "https://api.openai.com", "https://api.openai.com"),
        None
    );
}

fn entry(id: &str, shape: WireShape, base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        shape,
        base_url: base_url.into(),
        api_key_env: None,
        enabled: None,
        local: None,
    }
}

#[test]
fn provider_local_flag_explicit_beats_url_derivation() {
    // Loopback URL → derived local; explicit flag wins in both directions
    // (host.docker.internal is the containerized-gateway case, #15/#18).
    let loopback = entry("ollama", WireShape::OpenAi, "http://127.0.0.1:11434");
    let mut declared_local = entry("hostgw", WireShape::OpenAi, "https://ollama.corp.example");
    declared_local.local = Some(true);
    let mut declared_cloud = entry("tunnel", WireShape::OpenAi, "http://localhost:9999");
    declared_cloud.local = Some(false);
    let cfg = ProxyConfig {
        providers: vec![loopback, declared_local, declared_cloud],
        ..Default::default()
    };
    let resolved = cfg.resolve_providers();
    assert_eq!(resolved.len(), 3);
    assert!(resolved[0].local, "loopback URL derives local=true");
    assert!(
        resolved[1].local,
        "explicit local=true wins over HTTPS host"
    );
    assert!(
        !resolved[2].local,
        "explicit local=false wins over loopback"
    );
}

#[test]
fn provider_registry_resolves_valid_entries() {
    // enterprise#7: a new OpenAI-compatible provider is pure config. A
    // declared HTTPS entry is its own custom-host opt-in (no extra flag).
    let cfg = ProxyConfig {
        providers: vec![
            entry(
                "foundry",
                WireShape::OpenAi,
                "https://acme.services.ai.azure.com/",
            ),
            entry("local-vllm", WireShape::OpenAi, "http://127.0.0.1:8000"),
        ],
        ..Default::default()
    };
    let resolved = cfg.resolve_providers();
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].id, "foundry");
    assert_eq!(resolved[0].shape, WireShape::OpenAi);
    assert_eq!(
        resolved[0].base_url, "https://acme.services.ai.azure.com",
        "base_url must be normalized (trailing slash stripped)"
    );
    assert_eq!(resolved[1].base_url, "http://127.0.0.1:8000");
}

#[test]
fn provider_registry_skips_invalid_entries_without_killing_rest() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_ALLOW_INSECURE_HTTP_UPSTREAM");
    let cfg = ProxyConfig {
        providers: vec![
            // Shadows a built-in name → skipped.
            entry("openai", WireShape::OpenAi, "https://evil.example"),
            // Uppercase/slash ids are unusable as a path segment → skipped.
            entry("Bad/Id", WireShape::OpenAi, "https://ok.example"),
            // Non-loopback plaintext HTTP without the opt-in → skipped.
            entry("insecure", WireShape::OpenAi, "http://gw.corp.example"),
            // Duplicate id → first wins.
            entry("groq", WireShape::OpenAi, "https://api.groq.com"),
            entry("groq", WireShape::OpenAi, "https://other.example"),
        ],
        ..Default::default()
    };
    let resolved = cfg.resolve_providers();
    assert_eq!(resolved.len(), 1, "only the first 'groq' entry survives");
    assert_eq!(resolved[0].id, "groq");
    assert_eq!(resolved[0].base_url, "https://api.groq.com");
}

#[test]
fn provider_registry_respects_enabled_flag_and_trims_key_env() {
    let mut disabled = entry("foundry", WireShape::OpenAi, "https://f.example");
    disabled.enabled = Some(false);
    let mut keyed = entry("router", WireShape::Anthropic, "https://r.example");
    keyed.api_key_env = Some("  ROUTER_KEY  ".into());
    let cfg = ProxyConfig {
        providers: vec![disabled, keyed],
        ..Default::default()
    };
    let resolved = cfg.resolve_providers();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].api_key_env.as_deref(), Some("ROUTER_KEY"));
    assert_eq!(
        Upstreams {
            anthropic: String::new(),
            openai: String::new(),
            chatgpt: String::new(),
            gemini: String::new(),
            providers: resolved,
        }
        .provider_by_id("router")
        .map(|p| p.shape),
        Some(WireShape::Anthropic)
    );
}

#[test]
fn provider_entry_toml_round_trip() {
    // The `[[proxy.providers]]` TOML surface: shape names are lowercase,
    // api_key_env/enabled optional.
    let toml_src = r#"
        anthropic_upstream = "https://api.anthropic.com"

        [[providers]]
        id = "foundry"
        shape = "openai"
        base_url = "https://acme.services.ai.azure.com"
        api_key_env = "FOUNDRY_API_KEY"

        [[providers]]
        id = "claude-gw"
        shape = "anthropic"
        base_url = "https://gw.corp.example/anthropic"
        enabled = false
    "#;
    let cfg: ProxyConfig = toml::from_str(toml_src).expect("parse [[providers]]");
    assert_eq!(cfg.providers.len(), 2);
    assert_eq!(cfg.providers[0].shape, WireShape::OpenAi);
    assert_eq!(
        cfg.providers[0].api_key_env.as_deref(),
        Some("FOUNDRY_API_KEY")
    );
    assert_eq!(cfg.providers[1].enabled, Some(false));
    // Only the enabled entry resolves.
    assert_eq!(cfg.resolve_providers().len(), 1);
}

#[test]
fn routing_and_baseline_toml_round_trip() {
    // The `[proxy.routing]` + `[proxy.baseline]` TOML surface
    // (enterprise#13/#15). Absent tables must default to inactive.
    let toml_src = r#"
        [routing]
        enabled = true

        [routing.aliases]
        "acme/fast" = "foundry:gpt-4o-mini"

        [routing.tiers]
        fast = "foundry:phi-4"
        premium = ""

        [baseline]
        reference_model = "claude-opus-4.5"
        local_shadow_rate_per_mtok = 0.4
    "#;
    let cfg: ProxyConfig = toml::from_str(toml_src).expect("parse routing/baseline");
    assert!(cfg.routing.is_active());
    assert_eq!(
        cfg.routing.aliases.get("acme/fast").map(String::as_str),
        Some("foundry:gpt-4o-mini")
    );
    assert_eq!(
        cfg.routing.tiers.get("fast").map(String::as_str),
        Some("foundry:phi-4")
    );
    assert_eq!(
        cfg.baseline.reference_model.as_deref(),
        Some("claude-opus-4.5")
    );
    assert!((cfg.baseline.effective_local_shadow_rate() - 0.4).abs() < f64::EPSILON);

    let empty: ProxyConfig = toml::from_str("").expect("empty config");
    assert!(!empty.routing.is_active(), "absent [routing] = passthrough");
    assert_eq!(empty.baseline.reference_model, None);
    assert!(
        (empty.baseline.effective_local_shadow_rate() - DEFAULT_LOCAL_SHADOW_RATE_PER_MTOK).abs()
            < f64::EPSILON
    );

    // enabled=true with no rules is still inactive (nothing to apply).
    let enabled_only: ProxyConfig = toml::from_str("[routing]\nenabled = true").expect("parse");
    assert!(!enabled_only.routing.is_active());
}

#[test]
fn parse_route_target_shapes() {
    assert_eq!(
        parse_route_target("foundry:gpt-4o-mini"),
        Some((Some("foundry"), "gpt-4o-mini"))
    );
    assert_eq!(
        parse_route_target(" claude-haiku-4-5 "),
        Some((None, "claude-haiku-4-5"))
    );
    assert_eq!(parse_route_target(""), None);
    assert_eq!(parse_route_target("  "), None);
    assert_eq!(parse_route_target(":model"), None);
    assert_eq!(parse_route_target("provider:"), None);
}

#[test]
fn role_aggressiveness_defaults_to_off() {
    // Opt-in: a fresh config compresses no prose, so the proxy stays
    // byte-for-byte unchanged until an operator sets a value (#710).
    let cfg = ProxyConfig::default();
    // Isolate from a developer shell that may export the override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
    crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
    assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::System), None);
    assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::User), None);
}

#[test]
fn role_aggressiveness_reads_config_and_clamps() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
    crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
    let cfg = ProxyConfig {
        role_aggressiveness: RoleAggressiveness {
            system: Some(0.7),
            user: Some(1.5),
        },
        ..Default::default()
    };
    assert_eq!(
        cfg.resolved_role_aggressiveness(ProseRole::System),
        Some(0.7)
    );
    // Out-of-range config values are clamped into [0,1].
    assert_eq!(cfg.resolved_role_aggressiveness(ProseRole::User), Some(1.0));
}

#[test]
fn role_aggressiveness_env_overrides_config() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_PROXY_SYSTEM_AGGR", "0.25");
    let cfg = ProxyConfig {
        role_aggressiveness: RoleAggressiveness {
            system: Some(0.9),
            user: None,
        },
        ..Default::default()
    };
    assert_eq!(
        cfg.resolved_role_aggressiveness(ProseRole::System),
        Some(0.25),
        "env override must win over the configured value"
    );
    crate::test_env::remove_var("LEAN_CTX_PROXY_SYSTEM_AGGR");
}

#[test]
fn role_aggressiveness_ignores_blank_env() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_PROXY_USER_AGGR", "  ");
    let cfg = ProxyConfig {
        role_aggressiveness: RoleAggressiveness {
            system: None,
            user: Some(0.4),
        },
        ..Default::default()
    };
    assert_eq!(
        cfg.resolved_role_aggressiveness(ProseRole::User),
        Some(0.4),
        "a blank/garbage env value must fall back to config, not disable it"
    );
    crate::test_env::remove_var("LEAN_CTX_PROXY_USER_AGGR");
}

#[test]
fn live_compress_defaults_on_and_config_disables() {
    // #481: default ON (today's behaviour); a config `false` opts into the
    // meter-only mode. Isolate from a developer shell exporting the override.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_LIVE_COMPRESS");
    assert!(
        ProxyConfig::default().live_compresses(),
        "live_compress must default to true"
    );
    let cfg = ProxyConfig {
        live_compress: Some(false),
        ..Default::default()
    };
    assert!(!cfg.live_compresses());
}

#[test]
fn live_compress_env_overrides_config() {
    let _lock = crate::core::data_dir::test_env_lock();
    // env `off` wins over a config `true`.
    crate::test_env::set_var("LEAN_CTX_PROXY_LIVE_COMPRESS", "off");
    let cfg = ProxyConfig {
        live_compress: Some(true),
        ..Default::default()
    };
    assert!(!cfg.live_compresses(), "env off must win over config true");
    // A garbage env value is ignored → falls back to config.
    crate::test_env::set_var("LEAN_CTX_PROXY_LIVE_COMPRESS", "maybe");
    assert!(
        cfg.live_compresses(),
        "unparseable env must fall back to config, not flip the mode"
    );
    crate::test_env::remove_var("LEAN_CTX_PROXY_LIVE_COMPRESS");
}

#[test]
fn live_compress_exclude_defaults_to_serena() {
    // #481: an unset list protects Serena's code-reading tools, which return
    // source bodies but are mis-bucketed as `Search` by name.
    let cfg = ProxyConfig::default();
    assert!(cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
    assert!(cfg.is_tool_live_compress_excluded("Serena.search_for_pattern"));
    assert!(!cfg.is_tool_live_compress_excluded("ctx_shell"));
}

#[test]
fn live_compress_exclude_explicit_list_replaces_default() {
    // An explicit list narrows the exclusion (Serena no longer protected).
    let cfg = ProxyConfig {
        live_compress_exclude: Some(vec!["my_reader".into()]),
        ..Default::default()
    };
    assert!(cfg.is_tool_live_compress_excluded("acme_my_reader_v2"));
    assert!(!cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
}

#[test]
fn live_compress_exclude_empty_list_disables_protection() {
    // `[]` fully clears the exclusion (operator opts every tool back in).
    let cfg = ProxyConfig {
        live_compress_exclude: Some(vec![]),
        ..Default::default()
    };
    assert!(!cfg.is_tool_live_compress_excluded("mcp__serena__find_symbol"));
}

#[test]
fn compress_protect_unset_is_a_noop() {
    // #1150: the default protects nothing, so compression stays on for all.
    let cfg = ProxyConfig::default();
    assert!(!cfg.is_path_compress_protected("tests/golden/output.snap"));
    assert!(cfg.compress_protect_globs().is_empty());
}

#[test]
fn compress_protect_matches_basename_and_path_globs() {
    // `*.snap` matches by file name anywhere; `**/golden/**` targets a dir.
    let cfg = ProxyConfig {
        compress_protect: Some(vec!["*.snap".into(), "**/golden/**".into()]),
        ..Default::default()
    };
    assert!(cfg.is_path_compress_protected("a/b/c/output.snap"));
    assert!(cfg.is_path_compress_protected("output.snap"));
    assert!(cfg.is_path_compress_protected("tests/golden/case1.txt"));
    assert!(!cfg.is_path_compress_protected("src/main.rs"));
}

#[test]
fn compress_protect_normalises_backslashes() {
    // A Windows-style path still matches a forward-slash glob.
    let cfg = ProxyConfig {
        compress_protect: Some(vec!["**/fixtures/*".into()]),
        ..Default::default()
    };
    assert!(cfg.is_path_compress_protected("tests\\fixtures\\big.json"));
}

#[test]
fn compress_protect_skips_malformed_globs_without_disabling_rest() {
    // One bad pattern must not take the valid ones down with it.
    let cfg = ProxyConfig {
        compress_protect: Some(vec!["[".into(), "*.lock".into()]),
        ..Default::default()
    };
    assert!(cfg.is_path_compress_protected("Cargo.lock"));
}
