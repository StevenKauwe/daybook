#!/usr/bin/env python3
"""Prepare Daybook's default transcription model during a source install."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]


def matches_pin(path, spec):
    if not path.is_file() or path.stat().st_size != spec["bytes"]:
        return False
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest() == spec["sha256"]


def install_model(directory, spec):
    source = spec["source"]
    target = directory / Path(urlsplit(source).path).name
    if matches_pin(target, spec):
        print(f"Verified cached transcription model: {target}")
        return

    directory.mkdir(parents=True, exist_ok=True)
    print(f"Downloading {target.name} ({spec['bytes'] / 1_000_000:.0f} MB)…", flush=True)
    with tempfile.TemporaryDirectory(prefix=".download-", dir=directory) as staging:
        pending = Path(staging) / target.name
        subprocess.run(
            ["curl", "--fail", "--location", "--silent", "--show-error", "--retry", "2",
             "--connect-timeout", "20", "--proto", "=https", "--proto-redir", "=https",
             "--max-filesize", str(spec["bytes"]), "--output", str(pending), source],
            check=True,
        )
        if not matches_pin(pending, spec):
            raise SystemExit("Model download failed size or SHA-256 verification. Rerun just install to retry.")
        pending.replace(target)
    print(f"Installed verified transcription model: {target}")


if __name__ == "__main__":
    os.umask(0o077)
    pin = json.loads((ROOT / "speechworks-source.json").read_text())
    manifest = json.loads((ROOT / pin["destination"] / "models.lock.json").read_text())
    install_model(ROOT / "data/models", manifest["models"]["streaming-medium"])
