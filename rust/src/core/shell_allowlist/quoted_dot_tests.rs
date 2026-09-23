//! #1829: single-quoted text is data, so a lone `.` pipe segment inside it
//! (idiomatic jq) must not trip the eval/source guard. Real `.`/`source` at
//! command position stays blocked, including next to quote look-alikes.

use super::{allow, check_all_segments};

fn list() -> Vec<String> {
    allow(&["echo", "printf", "jq"])
}

#[test]
fn lone_dot_segment_inside_single_quotes_is_allowed() {
    for cmd in [
        "echo 'x | . | (b|c)'",
        "echo 'x | . | ($b)'",
        "printf '%s\\n' 'x | . | ($b|c)'",
        "echo 'x\n. as $r | ($r|z)'",
        "jq -s 'map(select(.prerelease==false) | . as $r | .assets[] | {series: ($r.tag|split(\".\")|.[0:2]|join(\".\"))})' /tmp/in.jsonl",
    ] {
        assert!(
            check_all_segments(cmd, &list()).is_ok(),
            "quoted jq program must not be blocked: {cmd}"
        );
    }
}

#[test]
fn dot_and_source_at_command_position_stay_blocked() {
    for cmd in [
        "echo 'a' | . /tmp/evil.sh",
        "echo 'a' ; source /tmp/evil.sh",
        "echo 'a'\n. /tmp/evil.sh",
        // A `'` that does not open a string must not hide what follows it.
        "echo \"it's\" ; . /tmp/evil.sh",
        "echo \\' ; . /tmp/evil.sh",
    ] {
        assert!(
            check_all_segments(cmd, &list()).is_err(),
            "`.`/`source` at command position must stay blocked: {cmd}"
        );
    }
}

#[test]
fn block_message_names_source_and_dot() {
    let err = check_all_segments("echo a | . /tmp/evil.sh", &list())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("source") && err.contains("`.`"),
        "message must name the construct that fired: {err}"
    );
}
