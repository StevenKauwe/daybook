#!/usr/bin/env python3
"""Verify authentication against a loopback-only model using generated sample keys."""
import argparse
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import threading
import time
from urllib.request import Request, urlopen
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--keychain", action="store_true", help="Create, rotate and remove one uniquely named test Keychain item")
args = parser.parse_args()
account = "Daybook test " + str(uuid.uuid4())
variable = "DAYBOOK_TEST_SUMMARY_" + uuid.uuid4().hex.upper()
samples = [secrets.token_hex(22) for _ in range(3)]
binary = Path(__file__).resolve().parents[1] / "dist/Daybook.app/Contents/MacOS/daybook"

class Model(BaseHTTPRequestHandler):
    seen = []
    expected = None

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        actual = self.headers.get("Authorization", "")
        self.seen.append(hmac.compare_digest(actual, "Bearer " + type(self).expected))
        result = json.dumps({"choices": [{"message": {"content": "Local test result"}}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(result)))
        self.end_headers()
        self.wfile.write(result)

    def log_message(self, *args):
        pass


with tempfile.TemporaryDirectory(prefix="daybook-provider-") as tmp, HTTPServer(("127.0.0.1", 0), Model) as model:
    threading.Thread(target=model.serve_forever, daemon=True).start()
    root = Path(tmp)
    child = subprocess.Popen([binary, "--synthetic", "--headless", "--data", root],
                             env={**os.environ, variable: samples[0]}, stdout=subprocess.DEVNULL)
    keychain_attempted = False
    try:
        address = root / "control-url.txt"
        for _ in range(160):
            if address.exists() and address.read_text().strip():
                break
            assert child.poll() is None, "Test recorder exited"
            time.sleep(.05)
        base = address.read_text().strip()

        def call(path, body=None):
            data = json.dumps(body).encode() if body is not None else None
            with urlopen(Request(base + path, data=data, headers={"Content-Type": "application/json"}), timeout=25) as response:
                return json.load(response)

        config = call("/overview")["config"]
        provider = config["summary"]
        provider.update(endpoint=f"http://127.0.0.1:{model.server_port}/v1", model="local-test")
        provider["api_key_env"] = variable
        call("/config", config)

        def summarize_once(sample):
            previous = len(Model.seen)
            Model.expected = sample
            call("/note", {"text": "Sample evidence " + str(previous)})
            call("/command", {"command": "compress"})
            for _ in range(120):
                if len(Model.seen) > previous:
                    break
                time.sleep(.05)
            assert len(Model.seen) == previous + 1 and Model.seen[-1], "Expected the generated credential on one local provider request"

        summarize_once(samples[0])
        print("PASS named environment variable authenticates a local summary", flush=True)
        if args.keychain:
            provider.update(api_key_env="", keychain_account=account)
            for key in samples[1:]:
                keychain_attempted = True
                reply = call("/config_with_key", {"config": config, "api_key": key})
                assert key not in json.dumps(reply)
                summarize_once(key)
            print("PASS new and replacement Mac Keychain keys authenticate local summaries", flush=True)
        for view in [call("/overview"), call("/status")]:
            assert all(key not in json.dumps(view) for key in samples)
        call("/command", {"command": "quit"})
        child.wait(8)
        for path in root.rglob("*"):
            if path.is_file():
                data = path.read_bytes()
                assert all(key.encode() not in data for key in samples), "A sample credential was written to journal files"
        print("PASS saved configuration, API responses and journal files contain only credential references", flush=True)
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(5)
        if keychain_attempted:
            result = subprocess.run(["/usr/bin/security", "delete-generic-password", "-s", "local.daybook.provider", "-a", account],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            # errSecItemNotFound is expected if a write was declined.
            assert result.returncode in (0, 44), "Couldn't clean up the temporary Daybook Keychain item"
        model.shutdown()
