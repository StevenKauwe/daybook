#!/usr/bin/env python3
"""Check a private pasteboard and recover its generated image through the signed app."""
from contextlib import contextmanager
from datetime import date
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time
from urllib.request import Request, urlopen
from urllib.error import HTTPError

BUNDLE = Path(__file__).resolve().parents[1] / "dist/Daybook.app/Contents/MacOS"


def wait_for(read):
    deadline = time.monotonic() + 125
    while time.monotonic() < deadline:
        result = read()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for the test journal")


@contextmanager
def journal(root):
    app = subprocess.Popen([BUNDLE / "daybook", "--synthetic", "--headless", "--data", root],
                           stdout=subprocess.DEVNULL)
    try:
        address = root / "control-url.txt"
        base = wait_for(lambda: address.exists() and address.read_text().strip())

        def call(path, data=None):
            body = None if data is None else json.dumps(data).encode()
            request = Request(base + path, data=body, headers={"Content-Type": "application/json"})
            with urlopen(request, timeout=5) as response:
                if response.headers.get_content_maintype() == "image":
                    assert response.headers["Cache-Control"] == "no-store"
                    return response.read()
                return json.load(response)

        assert call("/overview")["active"] == "false"
        yield call
        call("/command", {"command": "quit"})
        assert app.wait(timeout=10) == 0
    finally:
        if app.poll() is None:
            app.terminate()
            app.wait(timeout=10)


def verify(root):
    fixture = root / "fixture.png"
    subprocess.run([BUNDLE / "daybook-native", "--self-test-clipboard", fixture],
                   check=True, timeout=125)
    with journal(root):
        pass
    png = fixture.read_bytes()
    ids = []
    at = int(time.time())
    with sqlite3.connect(root / "daybook.sqlite") as db:
        for source, status in [("screenshot", "pending"), ("clipboard", "failed")]:
            details = {"saved_image": True, "width": 800, "height": 180, "ocr_status": status}
            row = db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) "
                             "VALUES(?,?,'screen',?,'',?)", (at, at, source, json.dumps(details)))
            ids.append(row.lastrowid)
            db.execute("INSERT INTO saved_images VALUES(?,?)", (row.lastrowid, png))
        jpeg = root / "images" / "periodic.jpg"
        subprocess.run(["/usr/bin/sips", "-s", "format", "jpeg", fixture, "--out", jpeg],
                       check=True, stdout=subprocess.DEVNULL)
        periodic = db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) "
                              "VALUES(?,?,'screen','screen','periodic fixture',?)",
                              (at, at, json.dumps({"path": "images/periodic.jpg"}))).lastrowid
    with journal(root) as call:
        for image_id, source in zip(ids, ["screenshot", "clipboard"]):
            if source == "clipboard":
                call("/retry_saved_ocr", {"id": image_id})
            def complete():
                item = call("/inspect", {"kind": "observation", "id": image_id})
                return item if item["details"]["ocr_status"] == "complete" else None

            item = wait_for(complete)
            assert item["image_retained"] and "Daybook synthetic decision" in item["text"]
            assert call(f"/image/{image_id}") == png
            page = call("/timeline", {"day": date.fromtimestamp(at).isoformat(), "kind": "screen",
                                      "source": source, "query": "synthetic decision"})
            assert [row["id"] for row in page["items"]] == [image_id]
            selected = call("/copy_time_range", {"day": date.fromtimestamp(at).isoformat(),
                                                 "from": at, "until": at + 1,
                                                 "kind": "screen", "source": source})
            assert selected["count"] == 1 and item["text"].strip() in selected["text"]
        assert call("/overview")["active"] == "false"
        assert call(f"/image/{periodic}") == jpeg.read_bytes()
        assert not (root / "saved-images").exists()
        jpeg.unlink()
        try:
            call(f"/image/{periodic}")
        except HTTPError as e:
            assert e.code == 410
        else:
            raise AssertionError("Expired image was served")
    print("PASS pending screenshot recovery, clipboard-image retry and searchable OCR while stopped")
    print("PASS inline viewing reads original PNGs and periodic JPEGs without exporting a file")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="daybook-saved-images-") as directory:
        verify(Path(directory))
