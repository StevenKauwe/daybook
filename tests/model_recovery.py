#!/usr/bin/env python3
"""Exercise the shipped recovery-loader patch using only local, prerecorded assets."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import sys

root = Path(__file__).resolve().parents[1]
model = Path(sys.argv[1]).resolve()
fixtures = Path(sys.argv[2]).resolve()
assert model.is_file() and (fixtures / "jfk.wav").is_file()
with tempfile.TemporaryDirectory(prefix="daybook-recovery-") as tmp:
    crate = Path(tmp) / "inference"
    shutil.copytree(root / "vendor/speechworks-8d2690a/crates/inference", crate)
    # Use Daybook's dependency versions and target, without writing into either Speechworks checkout.
    shutil.copyfile(root / "Cargo.lock", crate / "Cargo.lock")
    with (crate / "Cargo.toml").open("a") as f:
        f.write("\n[profile.release]\nopt-level = 2\n")
    env = dict(os.environ, RUSTC_WRAPPER="", CARGO_BUILD_JOBS="1",
               CARGO_TARGET_DIR=str(root / "target"), DAYBOOK_RECOVERY_MODEL=str(model),
               SPEECHWORKS_TEST_MODELS=str(fixtures))
    subprocess.run(["cargo", "test", "--offline", "--lib", "--release",
                    "file_recovery_replays_streaming_audio_after_device_loss", "--", "--ignored"],
                   cwd=crate, env=env, check=True)
