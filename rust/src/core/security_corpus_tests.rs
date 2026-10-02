use super::{detect_secrets, scan_and_redact};
use crate::core::{config::SecretDetectionConfig, redaction};
use base64::Engine as _;
use std::collections::BTreeMap;

struct Case {
    id: &'static str,
    class: &'static str,
    pieces: Vec<String>,
    positive: bool,
    supported: bool,
    reason: Option<&'static str>,
    secret: Option<String>,
    keep: Vec<&'static str>,
}

impl Case {
    fn supported(class: &'static str, id: &'static str, input: String, secret: String) -> Self {
        Self {
            id,
            class,
            pieces: vec![input],
            positive: true,
            supported: true,
            reason: None,
            secret: Some(secret),
            keep: vec!["KEEP_LEFT", "KEEP_RIGHT"],
        }
    }

    fn unsupported(
        class: &'static str,
        id: &'static str,
        input: String,
        secret: String,
        reason: &'static str,
    ) -> Self {
        Self {
            id,
            class,
            pieces: vec![input],
            positive: true,
            supported: false,
            reason: Some(reason),
            secret: Some(secret),
            keep: vec!["KEEP_LEFT", "KEEP_RIGHT"],
        }
    }

    fn negative(class: &'static str, id: &'static str, input: String) -> Self {
        Self {
            id,
            class,
            pieces: vec![input],
            positive: false,
            supported: true,
            reason: None,
            secret: None,
            keep: vec!["KEEP_LEFT", "KEEP_RIGHT"],
        }
    }

    fn chunked_unsupported(class: &'static str, id: &'static str, pieces: Vec<String>) -> Self {
        Self {
            id,
            class,
            pieces,
            positive: true,
            supported: false,
            reason: Some("each model-bound output chunk is scanned independently"),
            secret: None,
            keep: vec!["KEEP_LEFT", "KEEP_RIGHT"],
        }
    }
}

fn wrap(value: &str) -> String {
    format!("KEEP_LEFT|{value}|KEEP_RIGHT")
}

fn corpus() -> Vec<Case> {
    let long = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let aws_akia = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let aws_asia = concat!("AS", "IAIOSFODNN7EXAMPLE");
    let aws_secret = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0".to_string();
    let github_classic = concat!("gh", "p_", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123456789");
    let github_fine = format!("{}{}", concat!("github", "_pat_"), long);
    let slack = concat!("xoxb-1234567890-", "abcdefghijklmnopqrstuvwx");
    let stripe = concat!("sk", "_live_", "1234567890ABCDEFGHIJ");
    let openai = format!("{}{}", concat!("sk", "-"), long);
    let anthropic = format!("{}{}", concat!("sk", "-ant-"), long);
    let jwt = concat!(
        "eyJhbGciOiJIUzI1NiJ9",
        ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        ".c2lnbmF0dXJlLW5vdC1hLXJlYWwtdG9rZW4"
    );
    let key_value = long.to_string();
    let basic = base64::engine::general_purpose::STANDARD.encode("demo-user:fake-pass!!");
    let base64_secret = base64::engine::general_purpose::STANDARD
        .encode("synthetic secret payload with enough bytes for a base64 value");
    let db_password = "db-password-never-real-012345".to_string();
    let url_encoded = concat!("sk%2D", "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789%3D");
    let unicode_value = "S3cretValue0-0123456789ABCDEFGHIJKLMNOP".to_string();
    let line_split = format!("{}\n{}", &openai[..12], &openai[12..]);
    let chunk_token = concat!("xoxb-1234567890-", "abcdefghijklmnopqrstuvwx");
    let cases = vec![
        Case::supported(
            "aws",
            "aws_akia_id",
            wrap(&format!("AWS_ACCESS_KEY_ID={aws_akia}")),
            aws_akia.into(),
        ),
        Case::supported(
            "aws",
            "aws_asia_id",
            wrap(&format!("AWS_ACCESS_KEY_ID={aws_asia}")),
            aws_asia.into(),
        ),
        Case::supported(
            "aws",
            "aws_secret_access_key",
            wrap(&format!("AWS_SECRET_ACCESS_KEY={aws_secret}")),
            aws_secret,
        ),
        Case::supported(
            "github",
            "github_classic",
            wrap(&github_classic),
            github_classic.into(),
        ),
        Case::supported(
            "github",
            "github_fine_grained",
            wrap(&github_fine),
            github_fine,
        ),
        Case::supported("slack_stripe", "slack_bot", wrap(&slack), slack.into()),
        Case::supported("slack_stripe", "stripe_live", wrap(&stripe), stripe.into()),
        Case::supported(
            "openai_anthropic",
            "openai_key",
            wrap(&openai),
            openai.clone(),
        ),
        Case::supported(
            "openai_anthropic",
            "anthropic_key",
            wrap(&anthropic),
            anthropic,
        ),
        Case::supported("jwt", "jwt_three_segments", wrap(&jwt), jwt.into()),
        // Review regressions: letters-only values look like identifiers, and a
        // short Basic credential is still a credential.
        Case::supported(
            "auth_header",
            "bearer_letters_only",
            wrap("curl -H 'Bearer QWxhZGRpbkhlbGxvV29ybGQ'"),
            "QWxhZGRpbkhlbGxvV29ybGQ".into(),
        ),
        Case::supported(
            "auth_header",
            "basic_short_credential",
            wrap("Authorization: Basic dTpw"),
            "dTpw".into(),
        ),
        Case::supported(
            "key_value",
            "quoted_letters_only_api_key",
            wrap(r#"API_KEY="QwErTyUiOpAsDfGhJkLzXc""#),
            "QwErTyUiOpAsDfGhJkLzXc".into(),
        ),
        Case::supported(
            "private_key",
            "pem_rsa_private_key",
            // Markers are split so secret scanners never see a literal key header.
            concat!(
                "KEEP_LEFT\n-----BEGIN RSA PRIVATE KEY",
                "-----\nMIIEowIBAAKCAQEAFAKEKEYDATA\n-----END RSA PRIVATE KEY",
                "-----\nKEEP_RIGHT"
            )
            .into(),
            "MIIEowIBAAKCAQEAFAKEKEYDATA".into(),
        ),
        Case::supported(
            "private_key",
            "openssh_private_key",
            concat!(
                "KEEP_LEFT\n-----BEGIN OPENSSH PRIVATE KEY",
                "-----\nb3BlbnNzaC1rZXktdjEAAAAABGZha2U=\n-----END OPENSSH PRIVATE KEY",
                "-----\nKEEP_RIGHT"
            )
            .into(),
            "b3BlbnNzaC1rZXktdjEAAAAABGZha2U=".into(),
        ),
        Case::supported(
            "db_url",
            "postgres_password_url",
            wrap(&format!(
                "DATABASE_URL={}dbuser:{db_password}@db.example.test:5432/app",
                "postgres://"
            )),
            db_password,
        ),
        Case::supported(
            "authorization",
            "authorization_bearer",
            wrap(&format!("Authorization: Bearer {openai}")),
            openai.clone(),
        ),
        Case::supported(
            "authorization",
            "authorization_basic",
            wrap(&format!("Authorization: Basic {basic}")),
            basic,
        ),
        Case::supported(
            "dotenv",
            "dotenv_plain",
            wrap(&format!("API_KEY={key_value}")),
            key_value.clone(),
        ),
        Case::supported(
            "dotenv",
            "dotenv_export_single_quotes",
            wrap(&format!("export OPENAI_API_KEY = '{openai}'")),
            openai.clone(),
        ),
        Case::supported(
            "dotenv",
            "dotenv_double_quotes",
            wrap(&format!("API_KEY = \"{key_value}\"")),
            key_value.clone(),
        ),
        Case::supported(
            "serialization",
            "nested_json",
            format!(
                r#"{{"keep_left":"KEEP_LEFT","outer":{{"service":{{"api_key":"{key_value}"}}}},"keep_right":"KEEP_RIGHT"}}"#
            ),
            key_value.clone(),
        ),
        Case::supported(
            "serialization",
            "escaped_json",
            format!(
                r#"{{\"keep_left\":\"KEEP_LEFT\",\"api_key\":\"{key_value}\",\"keep_right\":\"KEEP_RIGHT\"}}"#
            ),
            key_value.clone(),
        ),
        Case::supported(
            "serialization",
            "nested_yaml",
            format!(
                "keep_left: KEEP_LEFT\nservices:\n  api:\n    token: '{key_value}'\nkeep_right: KEEP_RIGHT"
            ),
            key_value.clone(),
        ),
        Case::supported(
            "encoding",
            "base64_wrapped_secret",
            wrap(&format!("secret={base64_secret}")),
            base64_secret.clone(),
        ),
        Case::supported(
            "unicode",
            "zero_width_inside_aws_id",
            wrap("AWS_ACCESS_KEY_ID=AK\u{200b}IAIOSFODNN7EXAMPLE"),
            "AK\u{200b}IAIOSFODNN7EXAMPLE".into(),
        ),
        Case::supported(
            "multiline",
            "multiline_private_key",
            concat!(
                "KEEP_LEFT\n-----BEGIN PRIVATE KEY",
                "-----\nline-one-fake\nline-two-fake\n-----END PRIVATE KEY",
                "-----\nKEEP_RIGHT"
            )
            .into(),
            "line-one-fake\nline-two-fake".into(),
        ),
        Case::supported(
            "encoding",
            "url_encoded_provider_token",
            wrap(&format!("API_KEY={url_encoded}")),
            url_encoded.into(),
        ),
        Case::supported(
            "unicode",
            "nfkc_fullwidth_key_name",
            wrap(&format!("ＡＰＩ＿ＫＥＹ={unicode_value}")),
            unicode_value,
        ),
        Case::unsupported(
            "unicode",
            "confusable_provider_prefix",
            wrap("sк-abcdefghijklmnopqrstuvwx"),
            "sк-abcdefghijklmnopqrstuvwx".into(),
            "lookalike Cyrillic characters in provider prefixes are not mapped to ASCII",
        ),
        Case::unsupported(
            "multiline",
            "provider_token_split_across_lines",
            wrap(&line_split),
            openai.clone(),
            "provider token patterns do not join token fragments across line breaks",
        ),
        Case::chunked_unsupported(
            "chunk_boundary",
            "provider_token_split_across_chunks",
            vec![
                format!("KEEP_LEFT|{}", &chunk_token[..12]),
                format!("{}|KEEP_RIGHT", &chunk_token[12..]),
            ],
        ),
        Case::supported(
            "authorization",
            "shell_quoted_bearer_header",
            wrap(&format!("curl -H 'Authorization: Bearer {openai}'")),
            openai.clone(),
        ),
        Case::negative(
            "placeholders",
            "openai_docs_sk_ellipsis",
            wrap("OPENAI_API_KEY=sk-..."),
        ),
        Case::negative(
            "source_identifiers",
            "api_key_identifier",
            wrap("api_key_name"),
        ),
        Case::negative(
            "source_identifiers",
            "secret_key_function",
            wrap("getSecretKey()"),
        ),
        Case::negative(
            "source_identifiers",
            "method_property_reference",
            wrap("serverEnv.getStripeSecretKey"),
        ),
        Case::negative(
            "type_annotations",
            "typescript_generic",
            wrap("apiKey: Record<string, unknown>"),
        ),
        Case::negative(
            "type_annotations",
            "typescript_union",
            wrap("password: string | undefined"),
        ),
        Case::negative(
            "env_references",
            "shell_env_reference",
            wrap("$AWS_SECRET_ACCESS_KEY"),
        ),
        Case::negative(
            "env_references",
            "process_env_reference",
            wrap("process.env.TOKEN"),
        ),
        Case::negative(
            "env_references",
            "template_secret_reference",
            wrap("${{ secrets.X }}"),
        ),
        Case::negative(
            "placeholders",
            "your_api_key_placeholder",
            wrap("<your-api-key>"),
        ),
        Case::negative("placeholders", "xxxx_placeholder", wrap("xxxx")),
        // Bearer in prose and docs must survive the default redaction path.
        Case::negative(
            "bearer_prose",
            "bearer_authentication_prose",
            wrap("The API uses Bearer authentication for every call."),
        ),
        Case::negative(
            "bearer_prose",
            "bearer_tokens_prose",
            wrap("bearer tokens_are_rotated_daily by the gateway"),
        ),
        Case::negative(
            "bearer_prose",
            "bearer_doc_placeholder",
            wrap("curl -H 'Authorization-Hint: Bearer YOUR_TOKEN_HERE'"),
        ),
        Case::negative(
            "placeholders",
            "openai_docs_ellipsis",
            wrap("OPENAI_API_KEY=sk-..."),
        ),
        Case::negative("masked_values", "asterisk_mask", wrap("api_key=****")),
        Case::negative(
            "masked_values",
            "redacted_marker",
            wrap("api_key=[REDACTED]"),
        ),
        Case::negative(
            "masked_values",
            "redacted_authorization",
            wrap("Authorization: Basic [REDACTED]"),
        ),
        Case::negative(
            "numeric_config",
            "numeric_api_key_value",
            wrap("api_key=1234567890"),
        ),
        Case::negative(
            "uuid",
            "uuid_assignment",
            wrap("api_key=550e8400-e29b-41d4-a716-446655440000"),
        ),
        Case::negative(
            "lockfile_hashes",
            "integrity_hash",
            wrap("sha512-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789=="),
        ),
        Case::negative(
            "non_secret_literals",
            "boolean_config",
            wrap("secret=false"),
        ),
        Case::negative(
            "non_secret_literals",
            "short_local_value",
            wrap("api_key=local"),
        ),
        Case::negative(
            "non_secret_literals",
            "lockfile_version",
            wrap("version=1.2.3"),
        ),
    ];

    cases
}

#[derive(Default)]
struct Counts {
    n: usize,
    detected: usize,
    false_negatives: usize,
    false_positives: usize,
    redaction_misses: usize,
    unsupported: usize,
}

#[test]
fn security_corpus() {
    let config = SecretDetectionConfig {
        enabled: true,
        redact: true,
        ..SecretDetectionConfig::default()
    };
    let sensitivity_config = crate::core::sensitivity::SensitivityConfig {
        enabled: true,
        policy_floor: crate::core::sensitivity::SensitivityLevel::Secret,
        action: crate::core::sensitivity::FloorAction::Redact,
    };
    let cases = corpus();
    let mut counts = BTreeMap::<&str, Counts>::new();
    for case in &cases {
        let mut detector_hit = false;
        let mut direct_redacted = false;
        let mut scan_redacted = false;
        let mut sensitivity_redacted = false;
        let mut direct_outputs = Vec::new();
        let mut scan_outputs = Vec::new();
        let mut sensitivity_outputs = Vec::new();
        for piece in &case.pieces {
            let direct_matches = detect_secrets(piece);
            let (scan_output, scan_matches) = scan_and_redact(piece, &config);
            let direct_output = redaction::redact_text_if_enabled(piece);
            let sensitivity_output =
                crate::core::sensitivity::enforce_text(piece.clone(), None, &sensitivity_config)
                    .into_text();
            detector_hit |= !direct_matches.is_empty() || !scan_matches.is_empty();
            direct_redacted |= direct_output != *piece;
            scan_redacted |= scan_output != *piece;
            sensitivity_redacted |= sensitivity_output != *piece;
            direct_outputs.push(direct_output);
            scan_outputs.push(scan_output);
            sensitivity_outputs.push(sensitivity_output);
        }
        let any_signal = detector_hit || direct_redacted || scan_redacted || sensitivity_redacted;
        let entry = counts.entry(case.class).or_default();
        entry.n += 1;
        entry.detected += usize::from(detector_hit);
        entry.false_negatives += usize::from(case.positive && !detector_hit);
        entry.false_positives += usize::from(!case.positive && any_signal);
        entry.redaction_misses += usize::from(
            case.positive && !(direct_redacted && scan_redacted && sensitivity_redacted),
        );
        entry.unsupported += usize::from(case.positive && !case.supported);

        assert!(
            case.supported || case.reason.is_some_and(|reason| !reason.is_empty()),
            "unsupported case {} needs a reason",
            case.id
        );

        if case.positive && case.supported {
            assert!(
                detector_hit,
                "supported positive {} was not detected",
                case.id
            );
            assert!(direct_redacted, "model redactor missed {}", case.id);
            assert!(scan_redacted, "scan_and_redact missed {}", case.id);
            assert!(
                sensitivity_redacted,
                "sensitivity enforcement missed {}",
                case.id
            );
            for output in direct_outputs
                .iter()
                .chain(&scan_outputs)
                .chain(&sensitivity_outputs)
            {
                for keep in &case.keep {
                    assert!(output.contains(keep), "{} lost surrounding {keep}", case.id);
                }
                if let Some(secret) = &case.secret {
                    assert!(
                        !output.contains(secret.as_str()),
                        "{} leaked its secret span",
                        case.id
                    );
                }
                if matches!(case.id, "nested_json" | "escaped_json") {
                    assert!(
                        output.contains('{') && output.contains('}'),
                        "{} lost JSON structure",
                        case.id
                    );
                    assert!(
                        output.contains("api_key"),
                        "{} lost the nested key",
                        case.id
                    );
                }
                if case.id == "nested_yaml" {
                    assert!(
                        output.contains("services:"),
                        "{} lost YAML structure",
                        case.id
                    );
                }
                if case.id == "postgres_password_url" {
                    assert!(
                        output.contains("db.example.test") && output.contains("/app"),
                        "{} lost URL host/path",
                        case.id
                    );
                }
            }
        } else if case.positive {
            assert!(
                !any_signal,
                "unsupported case {} started being detected: {}",
                case.id,
                case.reason.unwrap_or("no reason")
            );
            for ((piece, direct), (scan, sensitivity)) in case
                .pieces
                .iter()
                .zip(&direct_outputs)
                .zip(scan_outputs.iter().zip(&sensitivity_outputs))
            {
                assert_eq!(
                    direct, piece,
                    "unsupported case {} changed in model redaction",
                    case.id
                );
                assert_eq!(
                    scan, piece,
                    "unsupported case {} changed in scanner redaction",
                    case.id
                );
                assert_eq!(
                    sensitivity, piece,
                    "unsupported case {} changed in sensitivity",
                    case.id
                );
            }
        } else {
            assert!(
                !any_signal,
                "negative case {} was falsely detected or redacted",
                case.id
            );
            for ((piece, direct), (scan, sensitivity)) in case
                .pieces
                .iter()
                .zip(&direct_outputs)
                .zip(scan_outputs.iter().zip(&sensitivity_outputs))
            {
                assert_eq!(
                    direct, piece,
                    "negative case {} changed in model redaction",
                    case.id
                );
                assert_eq!(
                    scan, piece,
                    "negative case {} changed in scanner redaction",
                    case.id
                );
                assert_eq!(
                    sensitivity, piece,
                    "negative case {} changed in sensitivity",
                    case.id
                );
            }
        }
    }

    println!(
        "security corpus summary (detected counts detector hits; FP includes detector or redactor signals)"
    );
    println!(
        "{:<22} {:>3} {:>8} {:>4} {:>4} {:>4} {:>11}",
        "class", "n", "detected", "FN", "FP", "RM", "unsupported"
    );
    let mut total = Counts::default();
    for (class, row) in counts {
        println!(
            "{class:<22} {:>3} {:>8} {:>4} {:>4} {:>4} {:>11}",
            row.n,
            row.detected,
            row.false_negatives,
            row.false_positives,
            row.redaction_misses,
            row.unsupported
        );
        total.n += row.n;
        total.detected += row.detected;
        total.false_negatives += row.false_negatives;
        total.false_positives += row.false_positives;
        total.redaction_misses += row.redaction_misses;
        total.unsupported += row.unsupported;
    }
    println!(
        "{:<22} {:>3} {:>8} {:>4} {:>4} {:>4} {:>11}",
        "TOTAL",
        total.n,
        total.detected,
        total.false_negatives,
        total.false_positives,
        total.redaction_misses,
        total.unsupported
    );
}
