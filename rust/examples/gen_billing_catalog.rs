fn main() {
    let catalog: Vec<_> = lean_ctx::core::billing::plans::Plan::all()
        .iter()
        .map(|p| p.entitlements())
        .collect();
    let json = serde_json::to_string_pretty(&catalog).expect("serialize billing catalog") + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/billing-plane-v2-catalog.json");
    std::fs::write(&path, &json).expect("write billing catalog");
    println!("Generated {} ({} bytes)", path.display(), json.len());
}
