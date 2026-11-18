#!/usr/bin/env python3
"""Bounded RAM overload/recovery through the real API, isolated synthetic data."""
import base64,json,pathlib,sqlite3,subprocess,tempfile,time,urllib.request
project=pathlib.Path(__file__).resolve().parents[1]
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-transcription-pressure-'))
(root/'config.json').write_text(json.dumps({'model_path':'','max_pending':2,'screen_mode':'off'}))
with (root/'process.log').open('w') as log:
    p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=log,stderr=log)
checks=[]
def check(name,ok):
    assert ok,name
    checks.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=12):
    until=time.monotonic()+seconds
    while time.monotonic()<until:
        v=fn()
        if v:return v
        time.sleep(.05)
    raise AssertionError('timed out waiting for fixture')
try:
    wait(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text()
    def api(path,body=None):
        req=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
        return json.load(urllib.request.urlopen(req,timeout=3))
    def block(i,track):
        at=time.time()
        return {'path':f'memory:888-{i}','track':track,'at':at,'end':at+.5,'frames':8000,'session':'fixture','sample_rate':16000,'pcm_base64':base64.b64encode(bytes(32000)).decode()}
    api('/command',{'command':'start'})
    api('/synthetic_audio',block(1,'me'));api('/synthetic_audio',block(2,'remote'))
    api('/synthetic_audio',block(3,'me'))
    live=api('/live')
    check('full RAM inbox preserves existing blocks and counts rejected audio',live['pending']==2 and live['memory_rejected_frames']==8000 and live['lost_sources']==1 and live['memory_audio']['pcm_bytes']==64000)
    check('overflow reason is visible while recording remains enabled',live['active']=='true' and live['armed']=='true' and '0.500 s' in live['capture_warning'])
    time.sleep(5.5) # Exercise the periodic capacity check that used to send Pause.
    check('periodic capacity check does not disarm a full RAM recorder',api('/live')['active']=='true')
    db=sqlite3.connect(root/'daybook.sqlite')
    check('lost source has durable timing and reason without PCM',db.execute("select count(*) from sources where path='memory:888-3' and status='lost' and error!=''").fetchone()[0]==1 and all('pcm_base64' not in row[0] for row in db.execute('select details from source_metadata')))
    # Simulate an inference consumer finishing; no device or model is involved.
    db.execute("update sources set status='discarded' where status='pending'");db.commit()
    wait(lambda:api('/live')['memory_audio']['pcm_bytes']==0)
    api('/synthetic_audio',block(4,'me'));api('/synthetic_audio',block(5,'remote'))
    check('both tracks admit new audio after space returns without Start',api('/live')['pending']==2 and api('/live')['active']=='true')
    sessions=dict(db.execute("select path,session from sources join source_sessions on source_id=id where path in ('memory:888-4','memory:888-5')"))
    check('only the track with missing audio gets a new recognition session',sessions=={'memory:888-4':'fixture:ram:1','memory:888-5':'fixture'})
    check('neither PCM nor audio sidecars are saved',not list((root/'audio').iterdir()))
    api('/command',{'command':'stop'})
    time.sleep(1)
    check('manual Stop remains stopped after overload recovery',api('/live')['active']=='false' and api('/live')['armed']=='false')
    api('/command',{'command':'quit'});p.wait(timeout=10)
    check('clean shutdown',p.returncode==0)
    print(json.dumps({'passed':len(checks),'fixture':str(root),'scope':'synthetic API and simulated inference completion; no capture hardware or remote providers'}))
finally:
    if p.poll() is None:p.terminate();p.wait(timeout=10)
