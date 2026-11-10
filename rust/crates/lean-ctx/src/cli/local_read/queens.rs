// SPDX-License-Identifier: Apache-2.0
//! `/v1/local/queens`: the Queen sessions LeanCTX | Hive Desktop started,
//! registered by the app over the 0600 local socket (socket only, no CLI
//! verb). A registered session may set task priority and labels through
//! `ctx_tasks` (`lean_ctx_hive::queens`).

use serde::Serialize;

use lean_ctx_hive::queens::{self, QueenSession};

use super::{Answer, Code, Failure};

#[derive(Debug, Serialize)]
struct QueensList {
    queens: Vec<QueenSession>,
}

fn session_id(args: &[String]) -> Result<&str, Failure> {
    super::flag(args, "--session-id")?.ok_or_else(|| Failure::invalid("session_id is required"))
}

fn local(error: lean_ctx_hive::HiveError) -> Failure {
    match error {
        lean_ctx_hive::HiveError::Config(message) => Failure::invalid(message),
        lean_ctx_hive::HiveError::Refused(message) => Failure::new(Code::Forbidden, message),
        other => Failure::new(Code::Unavailable, other.to_string()),
    }
}

fn build(args: &[String]) -> Result<QueensList, Failure> {
    let dir = queens::data_dir().map_err(local)?;
    let action = super::positionals(args).first().copied().unwrap_or("list");
    let queens = match action {
        "list" => queens::list_in(&dir),
        "register" => {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            queens::register_in(&dir, session_id(args)?, super::flag(args, "--name")?, &now)
                .map_err(local)?
        }
        "remove" => queens::remove_in(&dir, session_id(args)?).map_err(local)?,
        other => return Err(Failure::invalid(format!("queens {other}: unknown"))),
    };
    Ok(QueensList { queens })
}

/// The `queens.list` document of a socket request (the person: the app).
pub(crate) fn answer(args: &[String]) -> Answer {
    super::answer("queens.list", || build(args))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::routes;

    fn call(method: &str, path: &str, body: &[(&str, Value)]) -> Value {
        let route = routes::all()
            .find(|r| r.path == path && r.method.as_str() == method)
            .expect("a route");
        let inputs: Vec<(String, Value)> = body
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        serde_json::from_str(&routes::answer(route, None, &inputs).text).expect("ok")
    }

    /// The app registers a Queen session, lists it and removes it; a bad id
    /// is refused as an invalid argument.
    #[test]
    fn the_app_registers_lists_and_removes_queen_sessions() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let empty = call("GET", "/v1/local/queens", &[]);
        assert_eq!(empty["kind"], "queens.list");
        assert_eq!(empty["queens"], json!([]));
        let added = call(
            "POST",
            "/v1/local/queens",
            &[
                ("session_id", json!("sess-0f3a9c2e")),
                ("name", json!("LeanCTX core")),
            ],
        );
        assert_eq!(added["queens"][0]["session_id"], "sess-0f3a9c2e");
        assert_eq!(added["queens"][0]["name"], "LeanCTX core");
        let dir = lean_ctx_hive::queens::data_dir().expect("ok");
        assert!(lean_ctx_hive::queens::is_queen_in(&dir, "sess-0f3a9c2e"));
        let bad = call("POST", "/v1/local/queens", &[("session_id", json!("x y"))]);
        assert_eq!(bad["error"]["code"], "invalid_argument");
        let gone = call(
            "POST",
            "/v1/local/queens/remove",
            &[("session_id", json!("sess-0f3a9c2e"))],
        );
        assert_eq!(gone["queens"], json!([]));
        let commands = super::super::capabilities::commands_of(&routes::table_json());
        assert_eq!(
            commands["queens"],
            json!([
                {"verb": "list", "via": ["socket"]},
                {"verb": "register", "via": ["socket"]},
                {"verb": "remove", "via": ["socket"]}
            ])
        );
    }
}
