#!/usr/bin/env python3
"""Run one existing public speech fixture through the pinned Speechworks model."""
import array,json,pathlib,sqlite3,subprocess,tempfile,time,urllib.request,wave,sys
project=pathlib.Path(__file__).resolve().parents[1]
model=pathlib.Path(sys.argv[1]);wav=pathlib.Path(sys.argv[2]);root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-model-'))
p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
try:
 for _ in range(100):
  if (root/'config.json').exists() and (root/'control-url.txt').exists():break
  time.sleep(.1)
 url=(root/'control-url.txt').read_text();c=json.loads((root/'config.json').read_text());c['model_path']=str(model);c['retain_audio_hours']=1;c['audio_storage']='recovery'
 req=urllib.request.Request(url+'/config',data=json.dumps(c).encode(),headers={'Content-Type':'application/json'});urllib.request.urlopen(req).read()
 with wave.open(str(wav)) as f:
  assert (f.getframerate(),f.getnchannels(),f.getsampwidth())==(16000,1,2)
  pcm=array.array('h',f.readframes(160000));floats=array.array('f',(v/32768 for v in pcm))
 path=root/'audio/me-fixture.f32';path.write_bytes(floats.tobytes());db=sqlite3.connect(root/'daybook.sqlite');db.execute("INSERT INTO sources(at,end_at,track,path) VALUES(?,?,'me','audio/me-fixture.f32')",(int(time.time())-10,int(time.time())));db.commit()
 start=time.monotonic()
 for _ in range(1200):
  row=db.execute("SELECT text,source_id FROM observations WHERE kind='transcript'").fetchone()
  if row:break
  time.sleep(.1)
 else:raise AssertionError(db.execute('SELECT status,error FROM sources').fetchall())
 assert row[0].strip(),row
 print('PASS real Speechworks model:',row,'seconds',round(time.monotonic()-start,2))
 for _ in range(100):
  path=db.execute('SELECT path FROM sources WHERE id=?',(row[1],)).fetchone()[0]
  if path.endswith('.m4a'):break
  time.sleep(.1)
 assert path.endswith('.m4a'),path
 assert (root/path).exists();assert not (root/'audio/me-fixture.f32').exists()
 req=urllib.request.urlopen(url+'/source/'+str(row[1]));assert req.status==200 and req.headers['Content-Type']=='audio/mp4'
 print('PASS compressed retained audio and source download',root)
finally:
 p.terminate();p.wait(timeout=5)
