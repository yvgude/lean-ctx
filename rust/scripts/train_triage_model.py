#!/usr/bin/env python3
"""Train lean-ctx triage ONNX model using local Gemma4 as teacher.

Pipeline:
1. Generate teacher labels via Ollama/Gemma4 for diverse task prompts
2. Train a tiny linear classifier on the BERT-like token embeddings
3. Export to ONNX with the correct input signature for ModelLoader

Output: ~/.lean-ctx/models/triage-v1.onnx
"""

import json
import os
import struct
import sys
import time
from pathlib import Path
from typing import List, Tuple

import numpy as np

try:
    import onnx
    from onnx import TensorProto, helper
except ImportError:
    print("ERROR: pip install onnx onnxruntime numpy")
    sys.exit(1)

OLLAMA_URL = "http://localhost:11434/api/generate"
MODEL = "gemma4:e2b"
OUTPUT_DIR = Path.home() / ".lean-ctx" / "models"
OUTPUT_PATH = OUTPUT_DIR / "triage-v1.onnx"
MAX_SEQ_LEN = 64
HIDDEN_DIM = 128
NUM_OUTPUTS = 5  # intent, complexity, scope, reasoning_need, risk

TRAINING_TASKS = [
    # coding_fix tasks (intent > 0, complexity varies)
    "fix the null pointer exception in auth.rs",
    "resolve the deadlock in the connection pool",
    "fix type mismatch error in parser module",
    "patch the memory leak in the cache layer",
    "fix the off-by-one error in pagination",
    "resolve compilation error after dependency update",
    "fix the race condition in the task scheduler",
    "patch broken authentication flow",
    "fix the SQL injection vulnerability in the query builder",
    "resolve the CORS error in the API gateway",
    "fix flaky test in integration suite",
    "patch the buffer overflow in the protocol handler",
    "fix the infinite loop in the retry logic",
    "resolve the encoding error in unicode handling",
    "fix the missing import causing build failure",
    # refactoring tasks (high complexity, multi-file scope)
    "refactor the entire authentication module to use traits",
    "split the monolithic router into separate crate modules",
    "migrate from sync to async across the core pipeline",
    "refactor the knowledge graph to support multiple backends",
    "restructure the test suite for parallel execution",
    "extract common patterns into a shared utilities crate",
    "refactor the compression engine for tree-sitter AST modes",
    "migrate the config system from TOML to layered sources",
    "refactor the MCP server to support concurrent sessions",
    "split the CLI into subcommand crates",
    # explore/review tasks (intent < 0, low complexity)
    "explain the shadow comparison report format",
    "review PR #42 for potential issues",
    "what does the knowledge router do?",
    "show me how the triage engine works",
    "explain the CPAO metric calculation",
    "summarize recent changes in the proxy module",
    "review the error handling strategy",
    "what is the purpose of the value gate?",
    "explain the provider routing algorithm",
    "describe the session persistence mechanism",
    # deployment/config tasks (medium complexity, medium scope)
    "deploy the new version to staging",
    "update the CI pipeline for the new test matrix",
    "configure the rate limiter for production load",
    "set up monitoring alerts for the inference gateway",
    "update the Dockerfile for multi-stage builds",
    "configure backup schedule for the knowledge store",
    "deploy the dashboard to the CDN",
    "update the load balancer health check endpoints",
    # test tasks (low-medium complexity)
    "add unit tests for the triage engine",
    "write integration tests for the billing webhook",
    "add property-based tests for the compression codec",
    "create a benchmark for the BM25 index",
    "add snapshot tests for the CLI output",
    "write E2E tests for the decision loop",
    "add fuzz tests for the protocol parser",
    "create regression tests for issue #789",
    # high-risk tasks
    "delete the deprecated V1 API and all references",
    "migrate production database schema with zero downtime",
    "rotate all API keys across all environments",
    "upgrade the cryptographic library with breaking changes",
    "merge the experimental branch into main",
]

LABEL_PROMPT = """You are a task classification system. Classify the following developer task on 5 dimensions.
Output ONLY a JSON object with these exact fields (float values between -1.0 and 1.0):
- intent: positive=coding_change, negative=explore/review
- complexity: -1=trivial, 0=medium, 1=very_complex
- scope: -1=single_file, 0=multi_file, 1=cross_project
- reasoning_need: -1=mechanical, 0=moderate, 1=deep_reasoning
- risk: -1=safe, 0=moderate, 1=high_risk

Task: "{task}"

Reply with ONLY the JSON object, no other text."""


def query_teacher(task: str) -> dict:
    """Query Gemma4 via Ollama for task labels."""
    import urllib.request

    prompt = LABEL_PROMPT.format(task=task)
    payload = json.dumps({
        "model": MODEL,
        "prompt": prompt,
        "stream": False,
        "options": {"temperature": 0.1, "num_predict": 100},
    }).encode()

    req = urllib.request.Request(
        OLLAMA_URL,
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = json.loads(resp.read())

    response_text = data.get("response", "")
    # Extract JSON from response
    try:
        # Try direct parse
        start = response_text.find("{")
        end = response_text.rfind("}") + 1
        if start >= 0 and end > start:
            return json.loads(response_text[start:end])
    except (json.JSONDecodeError, ValueError):
        pass

    # Fallback: heuristic labels
    return heuristic_labels(task)


def heuristic_labels(task: str) -> dict:
    """Deterministic fallback labels based on keywords."""
    t = task.lower()
    intent = 0.8 if any(w in t for w in ["fix", "patch", "resolve", "add", "create", "write"]) else -0.6
    complexity = 0.7 if any(w in t for w in ["refactor", "migrate", "restructure", "entire"]) else (
        -0.3 if any(w in t for w in ["explain", "show", "what"]) else 0.2
    )
    scope = 0.8 if any(w in t for w in ["across", "entire", "all", "cross"]) else (
        -0.5 if any(w in t for w in ["single", "one", "this"]) else 0.1
    )
    reasoning = 0.6 if any(w in t for w in ["refactor", "design", "architect", "migrate"]) else (
        -0.4 if any(w in t for w in ["fix", "patch", "update"]) else 0.1
    )
    risk = 0.8 if any(w in t for w in ["delete", "migrate.*production", "rotate", "breaking"]) else (
        -0.5 if any(w in t for w in ["test", "review", "explain"]) else 0.0
    )
    return {"intent": intent, "complexity": complexity, "scope": scope,
            "reasoning_need": reasoning, "risk": risk}


def tokenize(text: str) -> Tuple[List[int], List[int], List[int]]:
    """Simple whitespace tokenizer matching the Rust ModelLoader's BertTinyTokenizer."""
    CLS, SEP, PAD, UNK = 101, 102, 0, 100
    VOCAB = {
        "fix": 8081, "bug": 11829, "test": 3231, "refactor": 10788,
        "config": 6149, "deploy": 21296, "review": 3319, "debug": 8567,
    }
    ids = [CLS]
    for token in text.lower().replace(",", " ").replace(".", " ").split():
        if len(ids) + 1 >= MAX_SEQ_LEN:
            break
        ids.append(VOCAB.get(token, UNK))
    ids.append(SEP)

    mask = [1] * len(ids)
    types = [0] * len(ids)

    # Pad to MAX_SEQ_LEN
    ids += [PAD] * (MAX_SEQ_LEN - len(ids))
    mask += [0] * (MAX_SEQ_LEN - len(mask))
    types += [0] * (MAX_SEQ_LEN - len(types))

    return ids[:MAX_SEQ_LEN], mask[:MAX_SEQ_LEN], types[:MAX_SEQ_LEN]


def build_training_data(use_teacher: bool = True) -> Tuple[np.ndarray, np.ndarray]:
    """Generate training data: X = token embeddings, Y = labels."""
    print(f"Generating labels for {len(TRAINING_TASKS)} tasks...")
    if use_teacher:
        print(f"Using teacher model: {MODEL} via Ollama")

    X_ids = []
    Y_labels = []
    failed = 0

    for i, task in enumerate(TRAINING_TASKS):
        if use_teacher:
            try:
                labels = query_teacher(task)
                print(f"  [{i+1}/{len(TRAINING_TASKS)}] {task[:50]}... → teacher")
            except Exception as e:
                labels = heuristic_labels(task)
                failed += 1
                print(f"  [{i+1}/{len(TRAINING_TASKS)}] {task[:50]}... → fallback ({e})")
        else:
            labels = heuristic_labels(task)
            print(f"  [{i+1}/{len(TRAINING_TASKS)}] {task[:50]}... → heuristic")

        ids, mask, types = tokenize(task)
        X_ids.append(ids)
        Y_labels.append([
            labels.get("intent", 0.0),
            labels.get("complexity", 0.0),
            labels.get("scope", 0.0),
            labels.get("reasoning_need", 0.0),
            labels.get("risk", 0.0),
        ])

    if failed > 0:
        print(f"\n  ({failed} tasks used heuristic fallback)")

    return np.array(X_ids, dtype=np.int64), np.array(Y_labels, dtype=np.float32)


def train_and_export(X: np.ndarray, Y: np.ndarray):
    """Train a simple linear model and export to ONNX.

    Architecture: Embedding(vocab, hidden) → MeanPool → Linear(hidden, 5)
    This matches the BERT-like signature expected by ModelLoader:
      inputs: input_ids[1,64], attention_mask[1,64], token_type_ids[1,64]
      output: logits[1,5]
    """
    import torch
    import torch.nn as nn

    VOCAB_SIZE = 30000
    EMBED_DIM = HIDDEN_DIM

    class TinyTriageModel(nn.Module):
        def __init__(self):
            super().__init__()
            self.embedding = nn.Embedding(VOCAB_SIZE, EMBED_DIM, padding_idx=0)
            self.linear = nn.Linear(EMBED_DIM, NUM_OUTPUTS)

        def forward(self, input_ids, attention_mask, token_type_ids):
            embeds = self.embedding(input_ids)  # [B, seq, dim]
            # Masked mean pooling
            mask_expanded = attention_mask.unsqueeze(-1).float()
            pooled = (embeds * mask_expanded).sum(dim=1) / mask_expanded.sum(dim=1).clamp(min=1)
            return self.linear(pooled)

    model = TinyTriageModel()
    optimizer = torch.optim.Adam(model.parameters(), lr=0.01)
    loss_fn = nn.MSELoss()

    X_tensor = torch.tensor(X, dtype=torch.long)
    Y_tensor = torch.tensor(Y, dtype=torch.float32)
    mask_tensor = (X_tensor != 0).long()
    types_tensor = torch.zeros_like(X_tensor)

    print("\nTraining tiny triage model...")
    model.train()
    for epoch in range(200):
        optimizer.zero_grad()
        output = model(X_tensor, mask_tensor, types_tensor)
        loss = loss_fn(output, Y_tensor)
        loss.backward()
        optimizer.step()
        if (epoch + 1) % 50 == 0:
            print(f"  Epoch {epoch+1}/200 — loss: {loss.item():.4f}")

    # Export to ONNX
    model.eval()
    dummy_ids = torch.zeros(1, MAX_SEQ_LEN, dtype=torch.long)
    dummy_mask = torch.ones(1, MAX_SEQ_LEN, dtype=torch.long)
    dummy_types = torch.zeros(1, MAX_SEQ_LEN, dtype=torch.long)

    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    print(f"\nExporting to {OUTPUT_PATH}...")
    torch.onnx.export(
        model,
        (dummy_ids, dummy_mask, dummy_types),
        str(OUTPUT_PATH),
        input_names=["input_ids", "attention_mask", "token_type_ids"],
        output_names=["logits"],
        dynamic_axes={
            "input_ids": {0: "batch"},
            "attention_mask": {0: "batch"},
            "token_type_ids": {0: "batch"},
            "logits": {0: "batch"},
        },
        opset_version=14,
    )

    # Verify
    model_onnx = onnx.load(str(OUTPUT_PATH))
    onnx.checker.check_model(model_onnx)
    size_mb = OUTPUT_PATH.stat().st_size / (1024 * 1024)
    print(f"\n  Model saved: {OUTPUT_PATH}")
    print(f"  Size: {size_mb:.2f} MB")
    print(f"  Inputs: {[inp.name for inp in model_onnx.graph.input]}")
    print(f"  Output: {[out.name for out in model_onnx.graph.output]}")

    # Quick inference test
    import onnxruntime as ort
    session = ort.InferenceSession(str(OUTPUT_PATH))
    test_task = "fix the authentication bug in src/auth.rs"
    ids, mask, types = tokenize(test_task)
    result = session.run(None, {
        "input_ids": np.array([ids], dtype=np.int64),
        "attention_mask": np.array([mask], dtype=np.int64),
        "token_type_ids": np.array([types], dtype=np.int64),
    })
    logits = result[0][0]
    print(f"\n  Test inference: '{test_task}'")
    print(f"  Logits: intent={logits[0]:.3f} complexity={logits[1]:.3f} "
          f"scope={logits[2]:.3f} reasoning={logits[3]:.3f} risk={logits[4]:.3f}")


def main():
    print("=" * 60)
    print("  lean-ctx Triage Model Training")
    print("  Teacher: Gemma4 (5.1B) via Ollama")
    print("  Target: ONNX INT8 classifier (~3MB)")
    print("=" * 60)

    # Check Ollama
    use_teacher = True
    try:
        import urllib.request
        urllib.request.urlopen("http://localhost:11434/api/tags", timeout=2)
        print("\n  Ollama: CONNECTED")
    except Exception:
        print("\n  Ollama: NOT AVAILABLE — using heuristic labels only")
        use_teacher = False

    X, Y = build_training_data(use_teacher=use_teacher)
    train_and_export(X, Y)
    print("\n" + "=" * 60)
    print("  DONE — model ready for lean-ctx")
    print("=" * 60)


if __name__ == "__main__":
    main()
