//! `lean-ctx addon init` scaffolding (P4 — lower the floor).
//!
//! Generates a ready-to-edit `lean-ctx-addon.toml` so an author starts from a
//! valid, secure-by-default manifest instead of a blank file. The output always
//! parses, validates and passes [`super::audit`] cleanly — guarded by a test —
//! so `addon init` → `addon audit` → `addon add ./lean-ctx-addon.toml` works end
//! to end on a fresh scaffold.

use crate::core::gateway::TransportKind;

/// The manifest filename an addon ships and `addon add <path>` expects.
pub const MANIFEST_FILENAME: &str = "lean-ctx-addon.toml";

/// Render a starter `lean-ctx-addon.toml` for `slug` and `transport`. Pure: the
/// caller decides where (and whether) to write it.
#[must_use]
pub fn addon_manifest(slug: &str, transport: TransportKind) -> String {
    let display = title_case(slug);
    let wiring = match transport {
        TransportKind::Stdio => format!(
            "[mcp]\n\
             transport = \"stdio\"\n\
             command = \"{slug}-mcp\"      # the executable that speaks MCP over stdio\n\
             args = [\"serve\"]\n\
             # env = {{ MY_TOKEN = \"...\" }}  # extra child-process env (avoid secrets here)\n\
             # sha256 = \"<shasum -a 256 {slug}-mcp>\"  # pin the binary (required for verified/paid)\n"
        ),
        TransportKind::Http => "[mcp]\n\
             transport = \"http\"\n\
             url = \"https://your-service.example/mcp\"   # streamable-HTTP MCP endpoint\n\
             # headers = { Authorization = \"Bearer ...\" }\n"
            .to_string(),
    };

    // A declared (secure-by-default) capability block: no network, read-only fs,
    // scrubbed env. Coherent with a local stdio tool; widen only what you need.
    let capabilities = match transport {
        TransportKind::Stdio => {
            "\n\
             [capabilities]\n\
             network = \"none\"          # \"full\" only if your tool calls the internet\n\
             filesystem = \"read_only\"  # \"read_write\" only if it writes outside a scratch tmp\n\
             env = []                   # host env var names your tool may receive\n\
             exec = \"none\"             # or [\"lean-ctx\"] if you spawn subprocesses (e.g. call back into lean-ctx)\n"
        }
        // An HTTP addon inherently uses the network; declaring it keeps the
        // audit coherent.
        TransportKind::Http => {
            "\n\
             [capabilities]\n\
             network = \"full\"          # an HTTP endpoint inherently uses the network\n"
        }
    };

    format!(
        "# lean-ctx addon manifest — see docs/guides/addons.md\n\
         # Validate before publishing:  lean-ctx addon audit ./{MANIFEST_FILENAME}\n\
         \n\
         [addon]\n\
         name = \"{slug}\"\n\
         display_name = \"{display}\"\n\
         description = \"One line describing what this addon does.\"\n\
         version = \"0.1.0\"\n\
         author = \"\"                  # your name or org (required to get listed)\n\
         homepage = \"\"                # repo / homepage URL (required to get listed)\n\
         license = \"Apache-2.0\"\n\
         categories = [\"workflow\"]\n\
         keywords = []\n\
         \n\
         {wiring}\
         {capabilities}"
    )
}

/// A slug derived from `name` (or a directory name): lowercase, non-alnum → `-`,
/// collapsed and trimmed. Returns `None` if nothing usable remains.
#[must_use]
pub fn slugify(name: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_end_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

fn title_case(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_ascii_uppercase().to_string() + c.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::addons::audit::{self, AuditVerdict};
    use crate::core::addons::manifest::AddonManifest;

    #[test]
    fn scaffold_stdio_is_valid_and_audits_clean() {
        let toml = addon_manifest("my-tool", TransportKind::Stdio);
        let m = AddonManifest::from_toml(&toml).expect("scaffold parses");
        m.validate().expect("scaffold validates");
        assert!(m.is_installable(), "stdio scaffold is installable");
        let report = audit::audit(&m);
        assert_eq!(report.verdict, AuditVerdict::Pass, "{:?}", report.findings);
        assert!(report.capability_coherent);
    }

    #[test]
    fn scaffold_http_is_valid_and_coherent() {
        let toml = addon_manifest("remote-svc", TransportKind::Http);
        let m = AddonManifest::from_toml(&toml).expect("scaffold parses");
        m.validate().expect("scaffold validates");
        let report = audit::audit(&m);
        assert!(
            report.capability_coherent,
            "http + network=full is coherent"
        );
        // HTTP endpoint is high-capability → review, never a fail.
        assert_ne!(report.verdict, AuditVerdict::Fail);
    }

    #[test]
    fn slugify_normalizes() {
        assert_eq!(slugify("My Cool Addon").as_deref(), Some("my-cool-addon"));
        assert_eq!(slugify("  weird__name!! ").as_deref(), Some("weird-name"));
        assert_eq!(slugify("already-good").as_deref(), Some("already-good"));
        assert_eq!(slugify("***").as_deref(), None);
    }

    #[test]
    fn slug_roundtrips_through_manifest_validation() {
        let slug = slugify("Acme Plans").unwrap();
        let m = AddonManifest::from_toml(&addon_manifest(&slug, TransportKind::Stdio)).unwrap();
        assert_eq!(m.addon.name, "acme-plans");
        assert_eq!(m.display_name(), "Acme Plans");
    }
}
