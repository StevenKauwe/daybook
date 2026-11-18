#!/usr/bin/env python3
"""Local inspection/UX contracts and stalled-provider isolation. No native capture."""
import pathlib,tempfile,subprocess,time,json,sqlite3,urllib.request,urllib.error,http.server,threading,wave,struct,math,datetime
project=pathlib.Path(__file__).resolve().parents[1];root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-quality-'));entered=threading.Event();release=threading.Event();p=None;checks=[]
class Provider(http.server.BaseHTTPRequestHandler):
 def do_POST(self):
  json.loads(self.rfile.read(int(self.headers['Content-Length'])));entered.set();release.wait(20)
  body=json.dumps({'choices':[{'message':{'content':'Local mock summary'}}],'usage':{'prompt_tokens':12,'completion_tokens':6}}).encode();self.send_response(200);self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass
server=http.server.HTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=server.serve_forever,daemon=True).start()
def check(name,v):
 assert v,name
 checks.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=12):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  v=fn()
  if v:return v
  time.sleep(.05)
 raise AssertionError('timeout')
def launch():
 global p,url
 (root/'control-url.txt').unlink(missing_ok=True);p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','a'),stderr=open(root/'stderr.log','a'));wait(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text()
def api(path,body=None):
 q=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
 return json.load(urllib.request.urlopen(q,timeout=5))
try:
 launch();c=api('/status')['config'];c['model_path']=str(project/'data/models/moonshine-tiny.gguf');c['summary'].update(endpoint=f'http://127.0.0.1:{server.server_port}/v1',model='mock',input_usd_per_million=0.1,output_usd_per_million=0.2);api('/config',c)
 api('/draft',{'text':'local unsaved thought'});check('draft is returned by lightweight overview',api('/overview')['draft']=='local unsaved thought')
 api('/note',{'text':'fixture-content-do-not-log'});api('/command',{'command':'compress'});check('background provider is running',entered.wait(5))
 with wave.open('/tmp/speechworks-demo/sample.wav','rb') as w:
  b=w.readframes(160000);pcm=b''.join(struct.pack('<f',x[0]/32768) for x in struct.iter_unpack('<h',b))
 (root/'audio/me-isolation.f32').write_bytes(pcm);db=sqlite3.connect(root/'daybook.sqlite');end=time.time()-1;cur=db.execute("INSERT INTO sources(at,end_at,track,path) VALUES(?,?,'me','audio/me-isolation.f32')",(int(end-10),int(end)));sid=cur.lastrowid;db.execute('INSERT INTO source_metadata VALUES(?,?)',(sid,json.dumps({'at':end-10,'end':end,'frames':160000,'sample_rate':16000,'track':'me','session':'synthetic-fixture','timing_estimated':False})));db.commit()
 started=time.monotonic();wait(lambda:db.execute("SELECT count(*) FROM observations WHERE kind='transcript'").fetchone()[0],8)
 check('transcription completes while provider remains blocked',not release.is_set() and db.execute("SELECT status FROM jobs").fetchone()[0]=='running');print('MEASURED blocked-provider transcription completion seconds',round(time.monotonic()-started,3),flush=True)
 h=api('/overview')['health'];check('latest source-end/final latency and compute time are available',h['final_latency']['source_end_to_result_ms']>0 and h['final_latency']['source_processing_ms']>0)
 check('macOS supplies recorder RSS and footprint, absent native telemetry and billing stay unknown',h['native'] is None and h['backend']['current_rss_bytes'] > 0 and h['backend']['physical_footprint_bytes'] > 0 and h['actual_billed_usd'] is None)
 release.set();wait(lambda:db.execute("SELECT count(*) FROM jobs WHERE status='done'").fetchone()[0]);h=api('/overview')['health'];a=h['activity'][0]
 check('provider activity records measured bytes, duration and reported usage',a['status']=='completed' and a['elapsed_ms']>0 and a['input_bytes']>0 and a['prompt_tokens']==12 and a['completion_tokens']==6)
 check('configured prices produce an explicitly estimated cost',math.isclose(a['estimated_usd'],0.0000024,abs_tol=1e-12) and h['usage']['priced_calls']==1 and h['usage']['reported_calls']==1)
 check('provider ledger excludes evidence text and credentials','fixture-content-do-not-log' not in json.dumps(h['activity']) and 'keychain' not in json.dumps(h['activity']))
 day=datetime.datetime.now().astimezone().date().isoformat();t=api('/timeline',{'day':day,'kind':'transcript','source':'me'});oid=t['items'][0]['id'];entry=api('/inspect',{'kind':'observation','id':oid});check('timeline inspection resolves retained source and capture metadata',entry['sources'][0]['id']==sid and entry['sources'][0]['retained'] and entry['sources'][0]['capture_metadata']['session']=='synthetic-fixture')
 job=api('/inspect',{'kind':'job','id':1});part=api('/inspect',{'kind':'part','id':1,'part':0});check('summary inspection resolves checkpoints and exact evidence input',job['parts'][0]['complete'] and job['evidence'] and 'fixture-content-do-not-log' in part['input'])
 try:api('/export_day',{'day':'../../secret'});raise AssertionError('invalid date accepted')
 except urllib.error.HTTPError as e:check('invalid export date rejected',e.code==400)
 check('selected daily artifact includes provenance','Source dates:' in api('/export_day',{'day':day})['text'])
 api('/command',{'command':'quit'});p.wait(timeout=10);launch();check('draft survives recorder restart',api('/overview')['draft']=='local unsaved thought')
 api('/note',{'text':'local unsaved thought'});check('saved note clears matching durable draft',api('/overview')['draft']=='')
 api('/command',{'command':'quit'});p.wait(timeout=10);check('clean shutdown',p.returncode==0);db.close();print(json.dumps({'passed':len(checks),'root':str(root)}),flush=True)
finally:
 release.set()
 if p and p.poll() is None:p.terminate();p.wait(timeout=10)
 server.shutdown()
