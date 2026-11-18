#!/usr/bin/env python3
"""Generated native PCM -> bounded meter publisher -> local API; no capture APIs."""
import json, pathlib, subprocess, tempfile, threading, time, urllib.request, sqlite3, sys

project = pathlib.Path(__file__).resolve().parents[1]
root = pathlib.Path(tempfile.mkdtemp(prefix='daybook-meter-test-'))
checks = []
def check(name, ok):
    assert ok, name
    checks.append(name); print('PASS', name, flush=True)
def wait(fn):
    until=time.monotonic()+10
    while time.monotonic()<until:
        value=fn()
        if value:return value
        time.sleep(.02)
    raise AssertionError('timeout')
def api(path, body=None):
    request=urllib.request.Request(url+path, data=None if body is None else json.dumps(body).encode(), headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request, timeout=3) as response:
        raw=response.read();return json.loads(raw)
def p95(values):
    ordered=sorted(values);return ordered[int(.95*(len(ordered)-1))]

p=subprocess.Popen([str(project/'target/release/daybook-local'),'--synthetic','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
native=None;ui=None;done=threading.Event();threads=[];errors=[];observed=[];latencies=[]
try:
    wait(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text()
    initial=api('/meters')
    check('stopped has no invented input',initial['active']=='false' and all(v is None for v in initial['health']['input_levels'].values()))
    api('/command',{'command':'start'})
    seconds=6 if '--ui' in sys.argv else 3
    if '--ui' in sys.argv:
        ui=subprocess.Popen([str(project/'ui-rust/target/release/daybook-ui'),url,'--profile-ui',str(root/'ui-profile.json')],stdout=open(root/'ui.log','w'),stderr=subprocess.STDOUT)
    def observe():
        while not done.is_set():
            began=time.perf_counter()
            try:
                value=api('/meters');observed.append((time.time(),(time.perf_counter()-began)*1000,value))
            except Exception as e:errors.append(str(e))
            done.wait(max(0,.016-(time.perf_counter()-began)))
    def health():
        while not done.is_set():
            try: api('/overview');api('/live')
            except Exception as e:errors.append(str(e))
            done.wait(.25)
    for fn in [observe,health]:
        t=threading.Thread(target=fn);t.start();threads.append(t)
    native=subprocess.Popen([str(project/'native-rust/target/release/daybook-native'),'--self-test-meters',str(seconds)],stdout=subprocess.PIPE,stderr=open(root/'native.log','w'),text=True)
    native_events=[];audio=[];sent_at=[]
    for line in native.stdout:
        event=json.loads(line)
        if event['type']=='meters':
            native_events.append(event);sent_at.append(time.monotonic())
            began=time.perf_counter();api('/synthetic_meters',event);latencies.append((time.perf_counter()-began)*1000)
        elif event['type']=='audio':audio.append({k:v for k,v in event.items() if k!='pcm_base64'})
    native.wait(timeout=3)
    check('native generated-signal helper exits cleanly',native.returncode==0)
    if ui:
        ui.wait(timeout=10);check('production UI probe closes cleanly',ui.returncode==0)
        profile=json.loads((root/'ui-profile.json').read_text())
        check('production UI sees at least 100 distinct microphone readings',profile['distinct_readings'][0]>=100)
        check('production UI microphone reading age p95 is below 120 ms',profile['meter_age_p95_ms'][0]<120)
        print('PRODUCTION_UI',json.dumps(profile),flush=True)
    done.set()
    for t in threads:t.join(timeout=5)
    check('both observers complete without errors',not errors)
    check('fast feed remains below one KiB',max(len(json.dumps(v)) for _,_,v in observed)<1024)
    native_span=sent_at[-1]-sent_at[0];rate=(len(sent_at)-1)/native_span
    check('native publishes 40–63 updates/sec',40<=rate<=63)
    for track in ['me','remote']:
        readings=[v['health']['input_levels'][track] for _,_,v in observed if v['health']['input_levels'][track]]
        check(track+' gets at least 50 distinct fresh observations',len({v['end_at'] for v in readings})>=50)
        expected_frames=160 if track=='me' else 320
        expected_method='microphone_io' if track=='me' else 'audio_consumer'
        check(track+' measures short native buffers independent of stored audio',all(v['frames']==expected_frames and v['sample_rate']==16000 and v['method']==expected_method for v in readings))
        if track=='remote':check('silent system channel never invents signal',all(v['rms_dbfs']==-120 and v['peak_dbfs']==-120 for v in readings))
        else:check('microphone pulses include real signal and silence',any(v['peak_dbfs']>-30 for v in readings) and any(v['rms_dbfs']==-120 for v in readings))
    ages=[(at-v['health']['input_levels']['me']['end_at'])*1000 for at,_,v in observed if v['health']['input_levels']['me']]
    check('observed signal age p95 stays under 80 ms',p95(ages)<80)
    check('audio blocks and frame totals stay unchanged',len(audio)==seconds*4 and all(sum(v['frames'] for v in audio if v['track']==track)==seconds*16000 for track in ['me','remote']))
    began=time.perf_counter();api('/command',{'command':'stop'});stopped=api('/meters');stop_ms=(time.perf_counter()-began)*1000
    api('/synthetic_meters',native_events[-1])
    check('Stop clears meters and late events cannot resurrect them',stopped['active']=='false' and all(v is None for v in api('/meters')['health']['input_levels'].values()))
    api('/command',{'command':'start'});api('/synthetic_meters',native_events[-1])
    check('restart rejects a preceding session',all(v is None for v in api('/meters')['health']['input_levels'].values()))
    api('/command',{'command':'stop'})
    db=sqlite3.connect(root/'daybook.sqlite')
    check('meter stream creates no persisted audio or transcript rows',db.execute('SELECT COUNT(*) FROM sources').fetchone()[0]==0 and db.execute("SELECT COUNT(*) FROM observations WHERE kind IN ('transcript','preview')").fetchone()[0]==0 and not list((root/'audio').iterdir()))
    print('MEASURED',json.dumps({'native_updates_per_second':rate,'native_events':len(sent_at),'sample_to_api_observation_p95_ms':p95(ages),'meter_ingest_api_p95_ms':p95(latencies),'stop_and_meter_clear_ms':stop_ms,'observer_samples':len(observed),'scope':'generated native signals through the production microphone meter, localhost observer at 60 Hz, concurrent transcript/health polling; excludes hardware callbacks and UI paint'}),flush=True)
    api('/command',{'command':'quit'});p.wait(timeout=10)
    check('clean backend shutdown',p.returncode==0)
    print(json.dumps({'passed':len(checks),'root':str(root)}),flush=True)
finally:
    done.set()
    for t in threads:t.join(timeout=5)
    for proc in [ui,native,p]:
        if proc and proc.poll() is None:proc.terminate();proc.wait(timeout=10)
