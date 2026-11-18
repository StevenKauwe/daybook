#!/usr/bin/env python3
"""Oversized vision and cross-day recovery against localhost models only."""
import pathlib,subprocess,tempfile,time,json,sqlite3,urllib.request,http.server,threading,datetime
project=pathlib.Path(__file__).resolve().parents[1];root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-review-'));p=None;inputs=[]
class Model(http.server.BaseHTTPRequestHandler):
 def do_POST(self):
  b=json.loads(self.rfile.read(int(self.headers['Content-Length'])));v=b['messages'][1]['content']
  if isinstance(v,list):text='🦀'*16000
  else:inputs.append(v);text='Summary checked: '+('recovered old-day evidence' if 'old-day' in v else 'local fixture')
  raw=json.dumps({'choices':[{'message':{'content':text}}]},ensure_ascii=False).encode();self.send_response(200);self.end_headers();self.wfile.write(raw)
 def log_message(self,*args):pass
mock=http.server.HTTPServer(('127.0.0.1',0),Model);threading.Thread(target=mock.serve_forever,daemon=True).start()
def wait(fn):
 for _ in range(200):
  value=fn()
  if value:return value
  time.sleep(.1)
 raise AssertionError('timeout')
def launch():
 global p,url
 (root/'control-url.txt').unlink(missing_ok=True)
 p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','a'),stderr=open(root/'stderr.log','a'))
 wait(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text()
def api(path,body=None):
 req=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(req,timeout=5))
try:
 launch();c=api('/status')['config'];c['summary'].update(endpoint=f'http://127.0.0.1:{mock.server_port}/v1',model='mock');c['vision']=dict(c['summary'],supports_images=True);c['daily_token_reservation']=1000000;c['rolling_audio_seconds']=86400;c['image_hours']=48;api('/config',c)
 db=sqlite3.connect(root/'daybook.sqlite');(root/'images/mock.jpg').write_bytes(b'mock-only');captured=int(time.time())-3600
 db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) VALUES(?,?,'screen','screen','',?)",(captured,captured,json.dumps({'path':'images/mock.jpg','vision_pending':True})));db.commit()
 wait(lambda:db.execute("SELECT count(*) FROM observations WHERE source='vision'").fetchone()[0])
 assert db.execute("SELECT at FROM observations WHERE source='vision'").fetchone()[0]==captured
 api('/note',{'text':'later action after large vision'});last=db.execute('SELECT max(id) FROM observations').fetchone()[0]
 def compress_to_end():
  api('/command',{'command':'compress'})
  return db.execute("SELECT count(*) FROM jobs WHERE status='done' AND kind='incremental' AND hi>=?",(last,)).fetchone()[0]
 wait(compress_to_end)
 assert all(len(x.encode())<=64000 for x in inputs)
 assert sum(x.count('🦀') for x in inputs)==16000
 assert any('later action after large vision' in x for x in inputs)
 assert db.execute("SELECT length(CAST(text AS BLOB)) FROM observations WHERE source='vision'").fetchone()[0]==64000
 print('PASS allowed 64,000-byte vision output reaches bounded checkpointed summaries without loss, then later note progresses',flush=True)
 p.kill();p.wait();yesterday=datetime.datetime.now().astimezone().date()-datetime.timedelta(days=1);old=int(datetime.datetime.combine(yesterday,datetime.time(12)).astimezone().timestamp())
 db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) VALUES(?,?,'note','me','old-day evidence','{}')",(old,old));oid=db.execute('SELECT max(id) FROM observations').fetchone()[0]
 db.execute("INSERT INTO jobs(kind,lo,hi,status,created) VALUES('incremental',?,?,'running',?)",(oid,oid,old));db.commit();launch()
 target=root/f'{yesterday}.txt';wait(lambda:target.exists() and 'recovered old-day evidence' in target.read_text())
 assert 'recovered old-day evidence' not in (root/f'{datetime.date.today()}.txt').read_text()
 print('PASS interrupted previous-day job resumes and updates its original daily artifact, without leaking into today',flush=True)
 api('/command',{'command':'quit'});p.wait(timeout=10);assert p.returncode==0;db.close();print('PASS clean shutdown',root,flush=True)
finally:
 if p and p.poll() is None:p.terminate();p.wait(timeout=10)
 mock.shutdown()
