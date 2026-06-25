//! `lean-ctx provider init` scaffolding (P4 — lower the floor).
//!
//! Generates a ready-to-edit config-provider TOML so an author starts from a
//! valid `[resources]` mapping instead of a blank file. The output always parses
//! and passes `ProviderConfig::validate` — guarded by a test — and lands in the
//! project-local `.lean-ctx/providers/` directory the discovery layer scans.

/// Project-local directory the discovery layer scans for provider configs.
pub const PROVIDERS_SUBDIR: &str = ".lean-ctx/providers";

/// Render a starter provider config TOML for `id`. Pure: the caller writes it.
#[must_use]
pub fn provider_config(id: &str) -> String {
    let display = title_case(id);
    let token_env = format!("{}_API_TOKEN", id.to_ascii_uppercase().replace('-', "_"));
    format!(
        "# lean-ctx config provider — see docs/guides/providers (config_provider).\n\
         # Drop this in .lean-ctx/providers/ and it is auto-discovered.\n\
         \n\
         id = \"{id}\"\n\
         name = \"{display}\"\n\
         base_url = \"https://api.example.com\"   # the REST API root\n\
         cache_ttl_secs = 120\n\
         \n\
         [auth]\n\
         type = \"bearer\"                        # none | bearer | api_key | basic | header\n\
         token_env = \"{token_env}\"             # env var holding the token\n\
         \n\
         # One entry per endpoint you want as context. Map the JSON response to\n\
         # lean-ctx item fields (id + title required; the rest optional).\n\
         [resources.items]\n\
         method = \"GET\"\n\
         path = \"/items\"\n\
         \n\
         [resources.items.query_params]\n\
         limit = \"{{limit}}\"                     # {{limit}}/{{state}} are interpolated at query time\n\
         \n\
         [resources.items.response]\n\
         root = \"data\"                          # dot-path to the array (omit if the root is the array)\n\
         \n\
         [resources.items.response.mapping]\n\
         id = \"id\"\n\
         title = \"name\"\n\
         # body = \"description\"\n\
         # url = \"html_url\"\n\
         # updated_at = \"updated_at\"\n"
    )
}

fn title_case(id: &str) -> String {
    id.split(['-', '_'])
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
    use crate::core::providers::config_provider::schema::ProviderConfig;

    #[test]
    fn scaffold_parses_and_validates() {
        let toml = provider_config("acme");
        let cfg: ProviderConfig = toml::from_str(&toml).expect("scaffold parses");
        cfg.validate().expect("scaffold validates");
        assert_eq!(cfg.id, "acme");
        assert_eq!(cfg.name, "Acme");
        assert!(cfg.resources.contains_key("items"));
    }

    #[test]
    fn token_env_is_derived_from_id() {
        let toml = provider_config("my-svc");
        assert!(toml.contains("MY_SVC_API_TOKEN"));
    }
}
