# Frozen Test Repository

Fixture for testing the lean-ctx indexing pipeline.

## Purpose

This repository captures a known set of source files to:

1. Verify pipeline output determinism
2. Detect regressions in signature extraction
3. Validate graph building and BM25 chunking

## Languages Included

- Rust (`src/main.rs`, `src/lib.rs`)
- Python (`src/utils.py`)
- TypeScript (`src/server.ts`, `src/handler.ts`)
- JavaScript (`src/config.js`)
- CSS (`src/styles.css`)
- HTML (`src/index.html`)
- Go (`src/main.go`)

## Usage

```bash
lean-ctx index build --root tests/fixtures/frozen-test-repo --mode full
```

## File Count

- Total files: 11 source files
- Languages: 7 programming languages + Markdown
