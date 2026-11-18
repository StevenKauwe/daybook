#!/usr/bin/env python3
"""Feed public prerecorded audio through two live-like file streams; no capture helper."""
import os,pathlib,tempfile,subprocess,json,time,sqlite3,urllib.request
project=pathlib.Path(__file__).resolve().parents[1];root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-streaming-app-'));pcm=pathlib.Path('/tmp/daybook-stream-fixture.f32').read_bytes()
p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
def wait(fn):
 for _ in range(300):
  v=fn()
  if v:return v
  time.sleep(.1)
 raise AssertionError('timeout')
try:
 wait(lambda:(root/'control-url.txt').exists());u=(root/'control-url.txt').read_text()
 def api(path,body=None):
  q=urllib.request.Request(u+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'});return json.load(urllib.request.urlopen(q,timeout=5))
 c=api('/status')['config'];c['model_path']=os.environ.get('DAYBOOK_TEST_MODEL',str(project/'data/models/moonshine-streaming-small-Q8_0.gguf'));api('/config',c);api('/command',{'command':'start'})
 db=sqlite3.connect(root/'daybook.sqlite');base=int(time.time());chunks=[pcm[i:i+64000] for i in range(0,len(pcm),64000)]
 def insert(i):
  for track in ['me','remote']:
   path=f'audio/{track}-{i}.f32';(root/path).write_bytes(chunks[i]);cur=db.execute('INSERT INTO sources(at,end_at,track,path) VALUES(?,?,?,?)',(base+i,base+i+1,track,path));db.execute('INSERT INTO source_sessions VALUES(?,?)',(cur.lastrowid,'synthetic-session'))
  db.commit()
 insert(0);insert(1)
 wait(lambda:db.execute("SELECT count(*) FROM observations WHERE kind='preview' AND json_extract(details,'$.incremental_encoder')=1").fetchone()[0]==2)
 assert db.execute("SELECT count(*) FROM observations WHERE kind='transcript'").fetchone()[0]==0
 api('/command',{'command':'compress'});assert not api('/status')['jobs']
 loaded=wait(lambda: (h if h and h.get('loaded') else None) if (h:=api('/overview')['health']['loaded_speech_model']) else None)
 assert loaded['model_path']==c['model_path'] and loaded['requested_backend']=='auto' and loaded['actual_backend'] in ['Metal','Cpu'],loaded
 if os.environ.get('DAYBOOK_EXPECT_BACKEND'):assert loaded['actual_backend']==os.environ['DAYBOOK_EXPECT_BACKEND'],loaded
 assert loaded['cpu_thread_cap']==2,loaded
 print('PASS actual loaded model/backend telemetry',json.dumps(loaded),flush=True)
 live=api('/overview')['health']['live_transcription'];assert len(live)==2 and all(r['kind']=='preview' for r in live),live
 print('PASS live overview exposes provisional text from both tracks',flush=True)
 print('PASS two independent incremental previews appear and are excluded from compression',flush=True)
 for i in range(2,len(chunks)):insert(i)
 api('/command',{'command':'stop'})
 wait(lambda:db.execute("SELECT count(*) FROM sources WHERE status!='done'").fetchone()[0]==0)
 rows=db.execute("SELECT source,text,details FROM observations WHERE kind='transcript'").fetchall();assert len(rows)==4,rows
 assert {r[0] for r in rows}=={'me','remote'} and all(r[1].strip() for r in rows)
 ids=[i for r in rows for i in json.loads(r[2])['source_ids']];assert len(ids)==len(chunks)*2 and len(set(ids))==len(ids)
 assert all(json.loads(r[2])['speechworks_revision'].startswith('8d2690a') for r in rows)
 wait(lambda:all(r['kind']=='transcript' for r in api('/overview')['health']['live_transcription']))
 print('PASS Stop publishes final text to the live overview',flush=True)
 print('PASS bounded phrases, Stop flush, nonempty finalized text and exact retained-source coverage',flush=True)
 api('/command',{'command':'compress'});wait(lambda:any(j['status']=='done' for j in api('/status')['jobs']))
 api('/command',{'command':'quit'});p.wait(timeout=10);assert p.returncode==0
 print('PASS finalized transcript compression and clean shutdown',root,flush=True)
finally:
 if p.poll() is None:p.terminate();p.wait(timeout=10)
