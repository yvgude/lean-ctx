use super::super::{install_mcp_json_agent, write_file, KIRO_STEERING_TEMPLATE};

pub(crate) fn install_kiro_hook() {
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();

    install_mcp_json_agent(
        "AWS Kiro",
        "~/.kiro/settings/mcp.json",
        &home.join(".kiro/settings/mcp.json"),
    );

    let cwd = std::env::current_dir().unwrap_or_default();
    let steering_dir = cwd.join(".kiro").join("steering");
    let steering_file = steering_dir.join("lean-ctx.md");

    if steering_file.exists()
        && std::fs::read_to_string(&steering_file)
            .unwrap_or_default()
            .contains("lean-ctx")
    {
        println!("  Kiro steering file already exists at .kiro/steering/lean-ctx.md");
    } else {
        let _ = std::fs::create_dir_all(&steering_dir);
        write_file(&steering_file, KIRO_STEERING_TEMPLATE);
        println!(
            "  \x1b[32m✓\x1b[0m Created .kiro/steering/lean-ctx.md (Kiro will now prefer lean-ctx tools)"
        );
    }
}
