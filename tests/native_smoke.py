#!/usr/bin/env python3
"""Launch the real native bridge stopped; inspect permissions without requesting capture."""
import pathlib,subprocess,tempfile,time,json,urllib.request
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-native-app-'));project=pathlib.Path(__file__).resolve().parents[1]
p=subprocess.Popen([str(project/'dist/Daybook.app/Contents/MacOS/daybook'),'--headless','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
try:
 for _ in range(100):
  if (root/'control-url.txt').exists():break
  if p.poll() is not None:raise AssertionError((root/'stderr.log').read_text())
  time.sleep(.1)
 url=(root/'control-url.txt').read_text()
 for _ in range(100):
  with urllib.request.urlopen(url+'/status') as r:s=json.load(r)
  if s['permissions']:break
  time.sleep(.1)
 assert s['permissions'],s
 assert s['active']=='false' and s['armed']=='false',s
 print('PASS real native bridge starts stopped; permission status:',s['permissions'],flush=True)
 for _ in range(100):
  with urllib.request.urlopen(url+'/overview') as r:h=json.load(r)['health']
  if h['native'] is not None:break
  time.sleep(.1)
 n=h['native'];assert n['queue_capacity_packets']==2048 and n['queue_capacity_pcm_bytes']==8*1024*1024 and n['queue_track_capacity_seconds']==5 and n['queue_frames']==0 and n['known_rejected_frames']==0,n
 assert n['device_dropped_frames'] is None and h['native_seen_at'] is not None,h
 print('PASS stopped native queue telemetry measured; device loss remains unavailable',flush=True)
 first=json.loads(s['permissions']).get('checked_at',0)
 req=urllib.request.Request(url+'/command',data=b'{"command":"permission_refresh"}',headers={'Content-Type':'application/json'});urllib.request.urlopen(req).read()
 for _ in range(50):
  with urllib.request.urlopen(url+'/status') as r:updated=json.load(r)
  if json.loads(updated['permissions']).get('checked_at',0)>first:break
  time.sleep(.1)
 assert json.loads(updated['permissions'])['checked_at']>first
 assert updated['active']=='false' and updated['armed']=='false'
 print('PASS permission refresh reports new sample without arming or capture',flush=True)
 req=urllib.request.Request(url+'/command',data=b'{"command":"quit"}',headers={'Content-Type':'application/json'});urllib.request.urlopen(req).read();p.wait(timeout=10)
 assert p.returncode==0
 print('PASS native bridge/backend shutdown without live capture',root,flush=True)
finally:
 if p.poll() is None:p.terminate();p.wait(timeout=10)
