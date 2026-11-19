#!/usr/bin/env python3
"""Exercise the installed UI's time-window API against a separate, stopped journal."""
import argparse
import base64
from datetime import datetime
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

parser = argparse.ArgumentParser()
parser.add_argument("--snapshot-dir", type=Path, help="Also render the native control window at one and ten minutes")
args = parser.parse_args()
bundle = Path(__file__).resolve().parents[1] / "dist/Daybook.app/Contents"
recorder = bundle / "MacOS/daybook"
controls = bundle / "Helpers/Daybook Controls.app/Contents/MacOS/daybook-ui"
png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+iRZkAAAAASUVORK5CYII=")

with tempfile.TemporaryDirectory(prefix="daybook-parallel-api-") as directory:
    root = Path(directory)
    process = subprocess.Popen([recorder, "--synthetic", "--headless", "--data", root], stdout=subprocess.DEVNULL)
    try:
        address = root / "control-url.txt"
        for _ in range(160):
            if address.exists() and address.read_text().strip():
                break
            if process.poll() is not None:
                raise RuntimeError("Test recorder exited during startup")
            time.sleep(.05)
        base = address.read_text().strip()

        def call(path, body=None):
            payload = json.dumps(body).encode() if body is not None else None
            with urlopen(Request(base + path, data=payload, headers={"Content-Type":"application/json"}), timeout=5) as response:
                return response.read() if path.startswith(("/image/","/passive_image/")) else json.load(response)

        overview = call("/overview")
        assert overview["synthetic"] and overview["active"] == "false"
        call("/config", dict(overview["config"], screen_seconds=10))
        assert call("/overview")["config"]["screen_seconds"] == 10
        assert json.loads((root / "config.json").read_text())["screen_seconds"] == 10
        first = int(datetime.now().replace(hour=10, minute=14, second=0, microsecond=0).timestamp())
        events = [
            (-487,-481,"transcript","me","Bring the paper samples to Market Street."),
            (-438,-430,"transcript","remote","We have time for another print proof."),
            (-408,-408,"note","clipboard","Ferry tickets · $125"),
            (-362,-356,"transcript","me","Keep the original cover in the studio."),
            (-335,-302,"screen","passive_screen","Proof corrections"),
            (-335,-286,"screen","passive_screen","Increase the title clearance to 4 mm"),
            (-201,-185,"screen","passive_screen","Thread samples · forest green"),
            (-303,-296,"transcript","remote","Saturday is the last press check."),
            (-260,-260,"note","clipboard","Proof deadline · Friday"),
            (-212,-206,"transcript","me","Ask the printer about the green thread."),
            (-166,-157,"transcript","remote","The cover needs another millimeter above the title."),
            (-90,-84,"transcript","me","I’ve got both copies here."),
            (3,10,"transcript","remote","How does the title look on the printed cover?"),
            (6,6,"screen","screenshot","Field Notes · Autumn edition"),
            (5,20,"screen","passive_screen","Field Notes · Proof 4"),
            (9,17,"transcript","me","Here it is. The title fits on one line, and the hills have a little more room."),
            (21,29,"transcript","remote","I like the space above the lettering. Keep the green."),
            (23,23,"note","clipboard","Stock: uncoated, warm white."),
            (35,42,"transcript","me","I’ll send the cover to print on Friday."),
            (42,50,"transcript","remote","Great. I’ll meet you at the press check."),
        ]
        with sqlite3.connect(root / "daybook.sqlite") as db:
            for start, end, kind, source, text in events:
                details = {"saved_image":True,"ocr_status":"done"} if source=="screenshot" else {}
                entry = db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) VALUES(?,?,?,?,?,?)",
                                   (first+start,first+end,kind,source,text,json.dumps(details))).lastrowid
                if source=="screenshot":
                    db.execute("INSERT INTO saved_images VALUES(?,?)",(entry,png))
                    image_id=entry

        window = call("/transcript_window", {})
        assert window["range"] == {"from":first, "until":first+60}
        assert len(window["passages"])==7 and len(window["nearby"])>len(window["passages"])
        assert call("/image/"+str(image_id))==png
        saved = [p for p in window["passages"] if p["source"]=="screenshot"]
        assert len(saved)==1 and saved[0]["image_id"]==image_id
        mode = call("/transcript_window", {"range": window["range"], "passive":True})
        assert all(p["source"]!="remote" for p in mode["passages"])
        assert any(p["source"]=="passive_screen" and p["end"]>p["at"] for p in mode["passages"])
        broad = call("/transcript_window", {"seconds":600})
        copy = call("/copy_transcript", {"range":broad["range"]})
        assert copy["count"]==sum(e[3] != "passive_screen" for e in events) and "$125" in copy["text"]
        print("PASS stopped launch, ordinary native window contract, local source switching, image provenance and complete copy", flush=True)
        axis = {"from":first-300, "until":first+300}
        fixed = call("/transcript_window", {"range":{"from":first+10, "until":first+27},"overview":axis})
        assert fixed["overview"] == axis and fixed["range"] == {"from":first+10,"until":first+27}
        visits = call("/screen_text", {})["items"]
        assert len(visits)==4 and all(v["end"] > v["at"] for v in visits)
        assert [v["text"] for v in call("/screen_text", {"query":"TITLE CLEARANCE"})["items"]] == ["Increase the title clearance to 4 mm"]
        print("PASS pinned overview and searchable automatic screen visits with first/last sightings", flush=True)
        assert call("/screen_captures", {})["last_check"] is None
        native=bundle / "MacOS/daybook-native"
        notes=root/"notes.png"
        studio=root/"studio.png"
        subprocess.run([native,"--make-ocr-fixture","1280","800","2",notes],check=True)
        subprocess.run(["/usr/bin/swift",Path(__file__).with_name("passive_screen_fixture.swift"),studio],check=True,timeout=90)
        def recognize(path):
            v=json.loads(subprocess.check_output([native,"--self-test-screen-frame",path],timeout=50))
            assert v["pixels"]==[1280,800] and v["jpeg"] and v["line_boxes"] and v["words"]
            return v
        note_frame, studio_frame=recognize(notes),recognize(studio)
        assert "Field Notes" in studio_frame["text"] and "Forest green thread" in studio_frame["text"]
        call("/command",{"command":"start"})
        for i in range(1,53):
            v=dict({1:note_frame,51:studio_frame}.get(i,{"type":"screen_unchanged"}))
            v["sample"]={"session":"fixture","target":"fixture","sequence":i,"at":first+32+(i-51)*10}
            call("/synthetic_screen",v)
        captures=call("/screen_captures",{"query":"forest green thread"})["items"]
        assert [item["at"] for item in captures] == [first+42,first+32]
        empty_search = call("/screen_captures", {"query":"no matching capture"})
        assert not empty_search["items"] and empty_search["last_check"] == first+42
        call("/command",{"command":"stop"})
        spatial = call("/screen_capture", {"id":captures[0]["id"]})
        assert spatial["image_available"] and spatial["layout"]["pixels"]==[1280,800]
        assert len(spatial["layout"]["lines"])==len(studio_frame["text"].splitlines())
        assert call("/passive_image/"+str(spatial["id"]))==base64.b64decode(studio_frame["jpeg"])
        passive=call("/transcript_window",{"range":{"from":first+32,"until":first+38},"passive":True})
        groups=[p for p in passive["passages"] if p["source"]=="passive_screen"]
        assert len(groups)==1 and "Forest green thread" in groups[0]["text"]
        assert groups[0]["capture_image"] == captures[1]["id"]
        assert call("/screen_capture",{"observation":groups[0]["id"]})["id"]==captures[1]["id"]
        assert not call("/screen_capture",{"id":2})["image_available"]
        try:
            call("/passive_image/2")
            raise AssertionError("An expired screenshot was still accessible")
        except HTTPError as e:
            assert e.code==410
        assert call("/passive_image/3")==base64.b64decode(note_frame["jpeg"])
        assert call("/image/"+str(image_id))==png
        earlier = call("/screen_captures", {"query":"title clearance"})["items"]
        archived = call("/screen_capture", {"id":earlier[0]["id"]})
        assert archived["legacy"] and len(archived["layout"]["lines"])==2
        print("PASS native JPEG and Vision geometry through recorder ingestion, latest-50 expiry, grouped provenance and intact manual saves", flush=True)
        if args.snapshot_dir:
            args.snapshot_dir.mkdir(parents=True, exist_ok=True)
            for name, flags in [("daybook-native-parallel.png",[]),("daybook-native-ten-minutes.png",["--ten-minutes"]),("daybook-native-screen-text.png",["--screen-history"])]:
                out = args.snapshot_dir / name
                subprocess.run([controls,"--design-preview",out,"--fixture-dir",root,*flags],check=True,timeout=50,stdout=subprocess.DEVNULL)
                assert out.stat().st_size>30_000
            print("PASS native screenshots from a running synthetic journal using the ordinary controls and recorder API", flush=True)
        assert call("/overview")["active"]=="false"
        call("/command",{"command":"quit"})
        process.wait(5)
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(5)
