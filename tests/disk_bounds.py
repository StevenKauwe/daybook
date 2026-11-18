#!/usr/bin/env python3
"""Synthetic cap test; no native helper or media capture."""
import pathlib, tempfile, subprocess, time, urllib.request, urllib.error, json
project=pathlib.Path(__file__).resolve().parents[1]
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-disk-cap-'))
p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
try:
 for _ in range(100):
  if (root/'control-url.txt').exists():break
  time.sleep(.1)
 url=(root/'control-url.txt').read_text()
 def api(path,body=None):
  req=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
  with urllib.request.urlopen(req,timeout=5) as r:return json.load(r)
 c=api('/status')['config'];c['disk_limit_mb']=64;api('/config',c)
 api('/command',{'command':'start'});assert api('/status')['active']=='true'
 fixture=root/'audio/cap.f32'
 with open(fixture,'wb') as f:f.truncate(64*1024*1024+1)
 for _ in range(100):
  s=api('/status')
  if s['active']=='false':break
  time.sleep(.1)
 assert s['active']=='false' and 'CAPACITY' in s['notice'],s
 assert fixture.exists(), 'capacity handling silently destroyed unexpired media'
 try:api('/command',{'command':'start'});raise AssertionError('over-cap restart accepted')
 except urllib.error.HTTPError as e:assert e.code==400
 print('PASS logical 64 MiB disk cap pauses active synthetic recorder, preserves media, reports capacity, rejects restart',flush=True)
 fixture.unlink();api('/command',{'command':'start'});assert api('/status')['active']=='true'
 api('/command',{'command':'stop'});api('/command',{'command':'quit'});p.wait(timeout=10)
 assert p.returncode==0
 print('PASS explicit restart after freeing capacity and clean shutdown',root,flush=True)
finally:
 if p.poll() is None:p.terminate();p.wait(timeout=10)
