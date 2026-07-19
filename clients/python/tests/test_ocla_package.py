from __future__ import annotations

import email
import json
import os
import shutil
import subprocess
import sys
import venv
import zipfile
from pathlib import Path
from typing import Dict, Optional


ROOT = Path(__file__).resolve().parents[3]
PYTHON_ROOT = ROOT / "clients" / "python"
FIXTURE = (
    ROOT
    / "clients"
    / "rust"
    / "lean-ctx-client"
    / "tests"
    / "fixtures"
    / "canonical-token-envelope-v1.json"
)
CONTRACT_SUITE = ROOT / "scripts" / "verify-ocla-contract-suite.py"
BUILD_REQUIREMENTS = Path(__file__).with_name(
    "ocla-build-requirements.lock"
)


def checked_run(
    arguments: list[str],
    cwd: Path,
    timeout: int = 120,
    extra_environment: Optional[Dict[str, str]] = None,
) -> subprocess.CompletedProcess[bytes]:
    environment = os.environ.copy()
    environment.pop("PYTHONPATH", None)
    if extra_environment is not None:
        environment.update(extra_environment)
    return subprocess.run(
        arguments,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=True,
    )


def test_built_wheel_is_dependency_free_and_console_entrypoint_runs(
    tmp_path: Path,
) -> None:
    source = tmp_path / "source"
    shutil.copytree(
        PYTHON_ROOT,
        source,
        ignore=shutil.ignore_patterns(
            "*.egg-info", "__pycache__", ".pytest_cache"
        ),
    )
    wheelhouse = tmp_path / "wheelhouse"
    wheelhouse.mkdir()
    build_tools = tmp_path / "build-tools"
    checked_run(
        [
            sys.executable,
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--require-hashes",
            "--only-binary=:all:",
            "--index-url",
            "https://pypi.org/simple",
            "--target",
            str(build_tools),
            "--requirement",
            str(BUILD_REQUIREMENTS),
        ],
        tmp_path,
    )
    checked_run(
        [
            sys.executable,
            "-m",
            "pip",
            "wheel",
            "--use-pep517",
            "--no-build-isolation",
            "--no-deps",
            "--wheel-dir",
            str(wheelhouse),
            str(source),
        ],
        tmp_path,
        extra_environment={"PYTHONPATH": str(build_tools)},
    )
    wheels = list(wheelhouse.glob("*.whl"))
    assert len(wheels) == 1
    wheel = wheels[0]
    with zipfile.ZipFile(wheel) as archive:
        metadata_name = next(
            name for name in archive.namelist() if name.endswith("/METADATA")
        )
        metadata = email.message_from_bytes(archive.read(metadata_name))
        runtime_requirements = [
            requirement
            for requirement in metadata.get_all("Requires-Dist", [])
            if "extra ==" not in requirement
        ]
        assert runtime_requirements == []
        entry_points_name = next(
            name
            for name in archive.namelist()
            if name.endswith("/entry_points.txt")
        )
        assert (
            b"leanctx-ocla-verify = leanctx.ocla_verify:main"
            in archive.read(entry_points_name)
        )

    environment = tmp_path / "installed"
    venv.EnvBuilder(with_pip=True, clear=True).create(environment)
    binary_directory = environment / ("Scripts" if os.name == "nt" else "bin")
    python = binary_directory / ("python.exe" if os.name == "nt" else "python")
    checked_run(
        [
            str(python),
            "-m",
            "pip",
            "install",
            "--no-index",
            "--no-deps",
            str(wheel),
        ],
        tmp_path,
    )
    verifier = binary_directory / (
        "leanctx-ocla-verify.exe"
        if os.name == "nt"
        else "leanctx-ocla-verify"
    )
    assert verifier.is_file()
    result = checked_run([str(verifier), "token", str(FIXTURE)], tmp_path)
    assert result.stdout == b"valid OCLA token envelope\n"
    assert result.stderr == b""
    suite = checked_run(
        [
            sys.executable,
            str(CONTRACT_SUITE),
            "--verifier",
            str(verifier),
        ],
        tmp_path,
    )
    report = json.loads(suite.stdout)
    assert report["all_passed"] is True
    assert report["certification_claimed"] is False
    assert len(report["results"]) == 18
