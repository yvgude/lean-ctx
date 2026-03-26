# lean-ctx — Token Optimization for Pi

lean-ctx is installed as a Pi Package. All bash, read, grep, find, and ls calls are automatically routed through lean-ctx for 60-90% token savings.

## How it works

- **bash** commands are compressed via lean-ctx's 90+ shell patterns
- **read** uses smart mode selection (full/map/signatures) based on file type and size
- **grep** results are grouped and compressed
- **find** and **ls** output is compressed and .gitignore-aware

## No manual prefixing needed

The Pi extension handles routing automatically. Just use tools normally:

```bash
git status          # automatically compressed
cargo test          # automatically compressed
kubectl get pods    # automatically compressed
```

## Checking status

Use `/lean-ctx` in Pi to verify which binary is active.

## Dashboard

Run `lean-ctx dashboard` in a separate terminal to see real-time token savings.
