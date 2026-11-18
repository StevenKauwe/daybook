#!/usr/bin/env python3
"""Check that UI appearance persists independently of recording and its settings."""
import json
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

binary = Path(__file__).resolve().parents[1] / "target/release/daybook-local"

def call(url, path, data=None):
    body = None if data is None else json.dumps(data).encode()
    req = Request(url + path, data=body, headers={"Content-Type": "application/json"})
    with urlopen(req, timeout=3) as response:
        return json.load(response)

with tempfile.TemporaryDirectory(prefix="daybook-reading-test-") as directory:
    root = Path(directory)
    choice = {"night": True, "journal": False, "size": 27}
    initial_config = None
    for restart in [False, True]:
        address = root / "control-url.txt"
        address.unlink(missing_ok=True)
        app = subprocess.Popen([str(binary), "--synthetic", "--headless", "--data", directory],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(150):
                if address.exists() and address.read_text().strip():
                    break
                time.sleep(.04)
            base = address.read_text().strip()
            overview = call(base, "/overview")
            if not restart:
                assert overview["reading"] == {"night": False, "journal": True, "size": 23}
                initial_config = overview["config"]
                call(base, "/reading", choice)
                assert call(base, "/overview")["reading"] == choice
                for invalid in [{**choice, "size": 240}, {**choice, "unknown": True}]:
                    try:
                        call(base, "/reading", invalid)
                    except HTTPError as e:
                        assert 400 <= e.code <= 499
                    else:
                        raise AssertionError("invalid preferences accepted")
                print("PASS missing preferences use paper, valid settings save, invalid sizes and fields rejected")
            overview = call(base, "/overview")
            assert overview["active"] == overview["armed"] == "false" and not overview["pending"]
            assert overview["reading"] == choice and overview["config"] == initial_config
            call(base, "/command", {"command": "quit"})
            app.wait(5)
        finally:
            if app.poll() is None:
                app.terminate()
                app.wait(5)
    print("PASS reading appearance survives relaunch with recording off and all recorder settings intact")
