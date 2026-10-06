#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/rust/Cargo.toml"
generated_sbom="$repo_root/rust/SBOM.cdx.json"
output_sbom="$repo_root/SBOM.cdx.json"

cleanup_generated_sboms() {
  find "$repo_root/rust" -type f -name 'SBOM.cdx.json' -delete
}
trap cleanup_generated_sboms EXIT
cleanup_generated_sboms

if ! cargo deny --version >/dev/null 2>&1; then
  echo "cargo-deny 0.19.6 is required" >&2
  exit 1
fi
cargo deny --manifest-path "$manifest_path" check licenses
cargo deny --manifest-path "$manifest_path" check advisories

cargo deny --manifest-path "$manifest_path" check
cargo cyclonedx --manifest-path "$manifest_path" --format json --target all \
  --override-filename SBOM.cdx
python3 - "$generated_sbom" <<'PY'
import hashlib
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
rust_root = path.parent.resolve()
absolute_prefix = f"path+file://{rust_root.as_posix()}"

def normalize(value):
    if isinstance(value, str):
        return value.replace(absolute_prefix, "path+file://./rust")
    if isinstance(value, list):
        return [normalize(item) for item in value]
    if isinstance(value, dict):
        return {key: normalize(item) for key, item in value.items()}
    return value

data = normalize(json.loads(path.read_text(encoding="utf-8")))
data.pop("serialNumber", None)
metadata = data.get("metadata", {})
metadata.pop("timestamp", None)
properties = [
    item
    for item in metadata.get("properties", [])
    if item.get("name") != "leanctx:cargo-lock-sha256"
]
properties.append({
    "name": "leanctx:cargo-lock-sha256",
    "value": hashlib.sha256((rust_root / "Cargo.lock").read_bytes()).hexdigest(),
})
metadata["properties"] = sorted(
    properties,
    key=lambda item: (item.get("name", ""), item.get("value", "")),
)
path.write_text(
    json.dumps(data, indent=2, sort_keys=True, separators=(",", ": ")) + "\n",
    encoding="utf-8",
)
PY
if [[ "${1:-}" == "--check" ]]; then
  if ! cmp -s "$generated_sbom" "$output_sbom"; then
    echo "SBOM.cdx.json is stale; run scripts/generate-sbom.sh" >&2
    exit 1
  fi
else
  mv "$generated_sbom" "$output_sbom"
fi
