from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "clients" / "rust" / "lean-ctx-client" / "tests" / "fixtures"
PYTHON_ROOT = ROOT / "clients" / "python"


def run_cli(*arguments: object) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, "-m", "leanctx.ocla_verify", *map(str, arguments)],
        cwd=PYTHON_ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=2,
        check=False,
    )


def test_cli_accepts_public_fixtures_without_identifiers_in_output() -> None:
    token = run_cli("token", FIXTURES / "canonical-token-envelope-v1.json")
    assert token.returncode == 0
    assert token.stdout == b"valid OCLA token envelope\n"
    assert token.stderr == b""
    agent = run_cli(
        "agent", FIXTURES / "agent-envelope-v1.json", "--gateway"
    )
    assert agent.returncode == 0
    assert agent.stdout == b"valid OCLA agent envelope\n"
    assert agent.stderr == b""


def test_cli_rejection_is_content_free() -> None:
    marker = b"request-1"
    result = run_cli("agent", FIXTURES / "canonical-token-envelope-v1.json")
    assert result.returncode == 2
    assert result.stdout == b""
    assert marker not in result.stderr
    assert result.stderr == (
        b"OCLA verification failed: invalid_or_unsafe_input\n"
    )


def test_cli_rejects_lone_unicode_surrogate_without_traceback(
    tmp_path: Path,
) -> None:
    wire = (FIXTURES / "canonical-token-envelope-v1.json").read_bytes()
    unsafe = tmp_path / "surrogate.json"
    unsafe.write_bytes(
        wire.replace(b'"provider":"openai"', b'"provider":"\\ud800"')
    )
    result = run_cli("token", unsafe)
    assert result.returncode == 2
    assert result.stdout == b""
    assert result.stderr == (
        b"OCLA verification failed: invalid_or_unsafe_input\n"
    )


@pytest.mark.parametrize(
    "wire",
    [
        b'{"schema_version":' + b"9" * 5000 + b"}",
        b"[" * 2000 + b"0" + b"]" * 2000,
    ],
)
def test_cli_rejects_bounded_parser_failures_without_traceback(
    tmp_path: Path, wire: bytes
) -> None:
    unsafe = tmp_path / "parser-limit.json"
    unsafe.write_bytes(wire)
    result = run_cli("token", unsafe)
    assert result.returncode == 2
    assert result.stdout == b""
    assert result.stderr == (
        b"OCLA verification failed: invalid_or_unsafe_input\n"
    )


@pytest.mark.skipif(os.name != "posix", reason="POSIX special-file contract")
def test_cli_rejects_fifo_without_open_block(tmp_path: Path) -> None:
    fifo = tmp_path / "wire.fifo"
    os.mkfifo(fifo)
    result = run_cli("token", fifo)
    assert result.returncode == 2
    assert result.stdout == b""


@pytest.mark.skipif(os.name != "posix", reason="POSIX symlink contract")
def test_cli_rejects_symlink_and_special_file(tmp_path: Path) -> None:
    link = tmp_path / "wire.json"
    link.symlink_to(FIXTURES / "canonical-token-envelope-v1.json")
    for path in (link, Path("/dev/null")):
        result = run_cli("token", path)
        assert result.returncode == 2
        assert result.stdout == b""


def test_cli_argument_errors_are_stable_and_content_free() -> None:
    for arguments in (
        (),
        ("other", FIXTURES / "canonical-token-envelope-v1.json"),
        (
            "token",
            FIXTURES / "canonical-token-envelope-v1.json",
            "--gateway",
        ),
        (
            "agent",
            FIXTURES / "agent-envelope-v1.json",
            "--unknown",
        ),
    ):
        result = run_cli(*arguments)
        assert result.returncode == 2
        assert result.stdout == b""
        assert result.stderr == (
            b"OCLA verification failed: invalid_or_unsafe_input\n"
        )
