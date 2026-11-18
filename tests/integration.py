#!/usr/bin/env python3
"""Synthetic local-only acceptance tests. Never starts native capture."""
import json, os, pathlib, sqlite3, subprocess, tempfile, time, urllib.request, urllib.error, threading, http.server
PROJECT=pathlib.Path(__file__).resolve().parents[1]
BIN=PROJECT/'target/release/daybook-local'
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-acceptance-'))
process=None
checks=[]
def check(name,condition):
    assert condition,name
    checks.append(name);print('PASS',name,flush=True)
def launch(*args):
    global process,url
    (root/'control-url.txt').unlink(missing_ok=True)
    process=subprocess.Popen([str(BIN),'--synthetic','--data',str(root),*args],stdout=open(root/'stdout.log','a'),stderr=open(root/'stderr.log','a'))
    for _ in range(100):
        if (root/'control-url.txt').exists():break
        if process.poll() is not None:raise RuntimeError((root/'stderr.log').read_text())
        time.sleep(.1)
    url=(root/'control-url.txt').read_text()
def api(path,body=None,headers=None):
    req=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json',**(headers or {})})
    try:
        with urllib.request.urlopen(req,timeout=5) as r:return r.status,json.loads(r.read())
    except urllib.error.HTTPError as e:return e.code,e.read().decode()
def command(c):return api('/command',{'command':c})
def wait_job(status='done',minimum=1):
    for _ in range(160):
        jobs=api('/status')[1]['jobs']
        if len([j for j in jobs if j['status']==status])>=minimum:return jobs
        time.sleep(.1)
    raise AssertionError(('job timeout',status,jobs))
class Mock(http.server.BaseHTTPRequestHandler):
    requests=0
    image_requests=0
    def do_POST(self):
        Mock.requests+=1
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.path=='/v1/chat/completions'
        assert body['model']=='synthetic-model'
        if isinstance(body['messages'][1]['content'],list):Mock.image_requests+=1
        response=json.dumps({'choices':[{'message':{'content':'Synthetic mock summary [source retained]. Action: review.'}}]}).encode()
        self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(response)
    def log_message(self,*args):pass
mock=None
try:
    view = {'page': 1, 'filter': {'day': '2026-11-17', 'sessions': True}}
    (root/'update').mkdir()
    (root/'update/workspace.json').write_text(json.dumps(view))
    launch('--after-update');check('starts stopped',api('/status')[1]['active']=='false')
    check('updated controls can restore their Archive page once',
          api('/resume_workspace', {}) == (200, view) and api('/resume_workspace', {}) == (200, None))
    check('paused ingestion rejected',api('/synthetic',{})[0]==400)
    check('cross-origin request rejected',api('/command',{'command':'start'},{'Origin':'https://evil.invalid'})[0]==403)
    check('start',command('start')[0]==200)
    check('synthetic transcript',api('/synthetic',{'source':'remote','text':'Decision: keep data local. Action: write a checklist.'})[0]==200)
    check('pause',command('pause')[0]==200 and api('/status')[1]['active']=='false')
    check('pause prevents ingestion',api('/synthetic',{})[0]==400)
    check('resume',command('start')[0]==200)
    api('/synthetic',{'source':'me','text':'I will review the checklist tomorrow.'});command('stop')
    c=api('/status')[1]['config'];bad=dict(c,screen_seconds=0)
    check('invalid config rejected atomically',api('/config',bad)[0]==400 and api('/status')[1]['config']['screen_seconds']==300)
    command('compress');jobs=wait_job();check('local summary has provenance',jobs[0]['lo']>=1 and 'Decision: keep data local' in jobs[0]['text'])
    command('compress');time.sleep(.3);check('repeat compression idempotent',len(api('/status')[1]['jobs'])==1)
    command('meta');wait_job(minimum=2);check('meta keeps original summary',len(api('/status')[1]['jobs'])==2)
    # Simulate a crash after claiming a new job, before a result is committed.
    process.kill();process.wait();db=sqlite3.connect(root/'daybook.sqlite')
    db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) VALUES(1,2,'note','me','Interrupted job evidence','{}')")
    hi=db.execute('SELECT max(id) FROM observations').fetchone()[0]
    lo=db.execute("SELECT max(hi)+1 FROM jobs WHERE kind='incremental'").fetchone()[0]
    db.execute("INSERT INTO jobs(kind,lo,hi,status,created) VALUES('incremental',?,?,'running',?)",(lo,hi,int(time.time())));db.commit();db.close()
    launch();jobs=wait_job(minimum=3);check('crash recovery finalizes interrupted job',any('Interrupted job evidence' in j['text'] for j in jobs));check('restart does not arm recorder',api('/status')[1]['active']=='false')
    mock=http.server.HTTPServer(('127.0.0.1',0),Mock);threading.Thread(target=mock.serve_forever,daemon=True).start()
    c=api('/status')[1]['config'];c['summary']['endpoint']=f'http://127.0.0.1:{mock.server_port}/v1';c['summary']['model']='synthetic-model';c['daily_requests']=1
    check('local provider config',api('/config',c)[0]==200)
    api('/note',{'text':'Mock provider evidence.'});command('compress');wait_job(minimum=4)
    check('Chat Completions mock called once',Mock.requests==1)
    api('/note',{'text':'Second budgeted job.'});command('compress');wait_job('failed');check('daily budget blocks second transmission',Mock.requests==1)
    command('retry');time.sleep(.8);check('retry cannot bypass persisted budget',Mock.requests==1)
    c['summary']['endpoint']='https://example.invalid/v1';check('hosted opt-in required',api('/config',c)[0]==400)
    c=api('/status')[1]['config'];c['daily_requests']=2;c['daily_images']=1;c['vision']=dict(c['summary'],supports_images=True);c['retain_images']=False
    check('independent vision config',api('/config',c)[0]==200)
    db=sqlite3.connect(root/'daybook.sqlite');db.execute("UPDATE jobs SET retry_at=? WHERE status='failed'",(int(time.time())+10000,))
    image=root/'images/synthetic.jpg';image.write_bytes(b'synthetic-image-for-mock-only')
    db.execute("INSERT INTO observations(at,end_at,kind,source,text,details) VALUES(?,?,'screen','screen','',?)",(int(time.time()),int(time.time()),json.dumps({'path':'images/synthetic.jpg','vision_pending':True})));db.commit()
    for _ in range(100):
        if Mock.image_requests and not image.exists():break
        time.sleep(.1)
    check('vision mock receives one image',Mock.image_requests==1)
    check('temporary image deleted after vision',not image.exists())
    row=db.execute("SELECT details FROM observations WHERE source='vision'").fetchone();check('vision links original observation',row is not None and json.loads(row[0])['observation_id']>0)
    check('persistent timeline search',api('/search',{'query':'checklist'})[1][0]['id']>0)
    db.close()
    check('daily text export',b'Summary #' in urllib.request.urlopen(url+'/export').read())
    command('quit');process.wait(timeout=5);check('quit exits and clears control capability',process.returncode==0 and not (root/'control-url.txt').exists())
    print(json.dumps({'passed':len(checks),'root':str(root),'checks':checks},indent=2))
finally:
    if process and process.poll() is None:process.terminate();process.wait(timeout=5)
    if mock:mock.shutdown()
