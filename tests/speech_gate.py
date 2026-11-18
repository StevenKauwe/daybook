#!/usr/bin/env python3
"""Speechworks VAD regression through a separate synthetic recorder, never live capture."""
import base64, json, pathlib, sqlite3, subprocess, tempfile, time, urllib.request

project=pathlib.Path(__file__).resolve().parents[1]
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-speech-gate-'))
pcm=pathlib.Path('/tmp/daybook-stream-fixture.f32').read_bytes()
passed=[]
def check(name,ok):
    assert ok,name
    passed.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=30):
    until=time.monotonic()+seconds
    while time.monotonic()<until:
        result=fn()
        if result:return result
        time.sleep(.02)
    raise AssertionError('condition timed out')
def api(path,body=None):
    req=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    return json.load(urllib.request.urlopen(req,timeout=10))
serial=0
def feed(track,chunk,at,session):
    global serial
    serial+=1
    api('/synthetic_audio',{'path':f'memory:444-{serial}','track':track,'at':at,'end':at+len(chunk)/64000,'frames':len(chunk)//4,'session':session,'sample_rate':16000,'pcm_base64':base64.b64encode(chunk).decode()})
    # Small local admission queue: wait for each source to be examined, not finalized.
    wait(lambda: db.execute("select count(*) from sources where status in ('pending','running')").fetchone()[0]==0)
def stop():
    api('/command',{'command':'stop'})
    wait(lambda: api('/live')['pending']==0)

p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
try:
    wait(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text().strip()
    db=sqlite3.connect(root/'daybook.sqlite')
    config=api('/overview')['config'];config['model_path']=str(project/'data/models/moonshine-streaming-medium-Q8_0.gguf');api('/config',config)
    wait(lambda:api('/overview')['health']['loaded_speech_model'].get('loaded'))
    api('/command',{'command':'start'});start=time.time()
    for i in range(80):
        for track in ['me','remote']: feed(track,bytes(32000),start+i*.5,'silence')
    stop()
    check('forty seconds of digital silence on both tracks produces no transcript or preview',db.execute("select count(*) from observations where kind in ('transcript','preview')").fetchone()[0]==0)
    check('silence does not invoke transcription',api('/overview')['health']['inference'] is None)
    check('all silent sources have explicit no-speech classification',db.execute("select count(*) from source_metadata where json_extract(details,'$.speech_gate')='no_speech'").fetchone()[0]==160)
    check('silence releases all admission PCM',api('/live')['memory_audio']['pcm_bytes']==0)

    api('/command',{'command':'start'});start=time.time();chunks=[pcm[i:i+32000] for i in range(0,len(pcm),32000)]
    for i,chunk in enumerate(chunks):
        feed('me',chunk,start+i*.5,'mixed')
        feed('remote',bytes(len(chunk)),start+i*.5,'mixed')
    for i in range(4):
        feed('me',bytes(32000),start+len(pcm)/64000+i*.5,'mixed')
        feed('remote',bytes(32000),start+len(pcm)/64000+i*.5,'mixed')
    check('a natural pause finalizes speech before Stop',db.execute("select count(*) from observations where kind='transcript' and source='me' and text!=''").fetchone()[0]>0 and db.execute("select count(*) from observations where kind='preview'").fetchone()[0]==0)
    check('silent system track stays empty while microphone speech transcribes',db.execute("select count(*) from observations where source='remote' and kind in ('transcript','preview')").fetchone()[0]==0)
    check('an uninterrupted utterance crosses the old eight-second boundary',db.execute("select max(end_at-at) from observations where kind='transcript' and source='me'").fetchone()[0]>8)
    rows=db.execute("select text,details from observations where kind='transcript'").fetchall()
    check('speech is recognized with source provenance',all(text.strip() and json.loads(details)['source_ids'] for text,details in rows))
    linked={sid for _,details in rows for sid in json.loads(details)['source_ids']}
    classified={sid for sid, in db.execute("select source_id from source_metadata where json_extract(details,'$.speech_gate')='no_speech'")}
    admitted={sid for sid, in db.execute('select id from sources')}
    check('every source is either linked to text or explicitly classified as no speech',linked|classified==admitted)
    stop();check('Stop drains all PCM without files',api('/live')['memory_audio']['pcm_bytes']==0 and not list((root/'audio').iterdir()))
    api('/command',{'command':'start'});start=time.time()+60
    for i,chunk in enumerate(chunks*3):feed('me',chunk,start+i*.5,'bounded-long-speech')
    stop()
    sizes=[]
    for details, in db.execute("select details from observations where kind='transcript'"):
        ids=json.loads(details)['source_ids']
        sizes.append(sum(db.execute("select json_extract(details,'$.frames') from source_metadata where source_id=?",(sid,)).fetchone()[0] for sid in ids))
    check('long continuous speech stays within the twenty-second model bound',max(sizes)<=320000)
    check('long speech leaves no backlog or PCM after Stop',api('/live')['pending']==0 and api('/live')['memory_audio']['pcm_bytes']==0)
    api('/command',{'command':'quit'});p.wait(timeout=10)
    check('synthetic backend exits cleanly',p.returncode==0)
    print(json.dumps({'passed':len(passed),'root':str(root),'mode':'synthetic fixtures only','speech_text':[text for text,_ in rows]}),flush=True)
finally:
    if p.poll() is None:p.terminate();p.wait(timeout=10)
