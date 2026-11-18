#!/usr/bin/env python3
"""Paced public audio -> RAM -> Speechworks -> live API. Never starts native capture."""
import pathlib,tempfile,subprocess,time,json,sqlite3,urllib.request,base64,threading,struct,math
project=pathlib.Path(__file__).resolve().parents[1]
root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-memory-stream-'))
pcm=pathlib.Path('/tmp/daybook-stream-fixture.f32').read_bytes()
p=None;checks=[]
def check(name,condition=True):
 assert condition,name
 checks.append(name);print('PASS',name,flush=True)
def wait(fn,seconds=20):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  v=fn()
  if v:return v
  time.sleep(.025)
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
 launch();c=api('/overview')['config'];check('memory storage is the default',c['audio_storage']=='memory')
 c['model_path']=str(project/'data/models/moonshine-streaming-medium-Q8_0.gguf');api('/config',c)
 warm=wait(lambda:(v if v['health']['loaded_speech_model'].get('loaded') else None) if (v:=api('/overview')) else None)
 check('model prepares while recording stays off',warm['active']=='false' and warm['pending']==0 and warm['health']['loaded_speech_model']['actual_backend']=='Metal')
 check('model load time is measured',warm['health']['loaded_speech_model']['load_ms']>=0)
 api('/command',{'command':'start'});db=sqlite3.connect(root/'daybook.sqlite')
 chunks=[pcm[i:i+32000] for i in range(0,len(pcm),32000)];base=time.time();observed=[];resource_samples=[];stop=threading.Event();poll_errors=[]
 def poll():
  while not stop.is_set():
   began=time.perf_counter()
   try:
    live=api('/live');observed.append((time.time(),(time.perf_counter()-began)*1000,live))
    if len(observed)%4==1:
     backend=api('/overview')['health']['backend'];resource_samples.append((backend['physical_footprint_bytes']/1048576,backend['cpu_seconds'],time.monotonic()))
   except Exception as e:poll_errors.append(str(e))
   stop.wait(.25)
 polling=threading.Thread(target=poll);polling.start()
 for i,chunk in enumerate(chunks):
  end=base+(sum(map(len,chunks[:i+1]))/64000)
  time.sleep(max(0,end-time.time()))
  samples=struct.unpack('<'+'f'*(len(chunk)//4),chunk);dbfs=lambda x:max(-120.0,min(0.0,20*math.log10(max(x,1e-6))))
  level={'rms_dbfs':dbfs(math.sqrt(sum(x*x for x in samples)/len(samples))),'peak_dbfs':dbfs(max(abs(x) for x in samples))}
  for t,track in enumerate(['me','remote']):
   api('/synthetic_audio',{'path':f'memory:101-{t}-{i}','track':track,'at':end-len(chunk)/64000,'end':end,'frames':len(chunk)//4,'session':'memory-public-fixture','sample_rate':16000,'timing_estimated':False,'level':level,'pcm_base64':base64.b64encode(chunk).decode()})
 stop_at=time.perf_counter();api('/command',{'command':'stop'})
 final=wait(lambda: (v if v['pending']==0 and v['live_transcription'] and all(x['kind']=='transcript' for x in v['live_transcription']) else None) if (v:=api('/live')) else None)
 drain_ms=(time.perf_counter()-stop_at)*1000;stop.set();polling.join()
 check('both tracks publish nonempty previews during paced capture',all(any(any(r['track']==track and r['kind']=='preview' and r['text'].strip() for r in v['live_transcription']) for _,_,v in observed) for track in ['me','remote']))
 check('both input meters publish measured values',all(any((v.get('input_levels',{}).get(track) or {}).get('peak_dbfs',-120)>-60 for _,_,v in observed) for track in ['me','remote']))
 check('reader page contains saved nonempty text',len(final['transcript']['items'])>=2 and all(r['text'].strip() for r in final['transcript']['items']))
 check('Stop drains the queue and finalizes both tracks',final['pending']==0)
 check('PCM buffer is empty after finalization',final['memory_audio']['pcm_bytes']==0)
 check('no audio or sidecar files created',not list((root/'audio').iterdir()))
 check('no PCM payload persisted in source metadata',db.execute("SELECT count(*) FROM source_metadata WHERE json_extract(details,'$.pcm_base64') IS NOT NULL").fetchone()[0]==0)
 check('all source blocks discarded after final text',db.execute("SELECT count(*) FROM sources WHERE status!='discarded'").fetchone()[0]==0)
 ids=[i for (d,) in db.execute("SELECT details FROM observations WHERE kind='transcript'") for i in json.loads(d)['source_ids']]
 check('final text retains exact source coverage',len(ids)==2*len(chunks) and len(set(ids))==len(ids))
 check('fast live polling completed without errors',not poll_errors)
 nonempty=[at-base for at,_,v in observed if any(r['text'].strip() for r in v['live_transcription'])]
 preview=json.loads(db.execute("SELECT value FROM state WHERE key='speech_preview_metrics'").fetchone()[0])
 times=sorted(ms for _,ms,_ in observed)
 print('MEASURED',json.dumps({'backend_sampled_footprint_mib':[round(min(s[0] for s in resource_samples),1),round(max(s[0] for s in resource_samples),1)],'backend_cpu_percent_of_one_core':round(100*(resource_samples[-1][1]-resource_samples[0][1])/(resource_samples[-1][2]-resource_samples[0][2]),1),'model_load_ms':warm['health']['loaded_speech_model']['load_ms'],'fixture_audio_seconds':len(pcm)/64000,'first_nonempty_observed_seconds':min(nonempty),'stop_to_final_ms':drain_ms,'live_api_p95_ms':times[int(.95*(len(times)-1))],'latest_source_end_to_preview_ms':preview['source_end_to_result_ms'],'max_ram_pcm_bytes':max(v['memory_audio']['pcm_bytes'] for _,_,v in observed),'max_pending_blocks':max(v['pending'] for _,_,v in observed),'actual_backend':api('/overview')['health']['loaded_speech_model']['actual_backend'],'scope':'public prerecorded audio, paced input and 250ms API observer; excludes native microphone capture and GUI paint'}),flush=True)
 drained_footprints=[]
 for _ in range(8):
  time.sleep(.4)
  settled=api('/overview')
  assert settled['active']=='false' and settled['pending']==0 and settled['health']['loaded_speech_model']['loaded']
  drained_footprints.append(round(settled['health']['backend']['physical_footprint_bytes']/1048576,1))
 print('AFTER_DRAIN',json.dumps({'seconds_after_stop':3.2,'recorder_footprint_mib':drained_footprints,'scope':'same warm backend after decoding both tracks, model remains on Metal'}),flush=True)
 # A failed model leaves PCM queued. Simulate process crash; recovery must report lost RAM,
 # preserve completed text, and never pretend audio is available on disk.
 c['model_path']='/missing/model.gguf';api('/config',c);api('/command',{'command':'start'})
 api('/synthetic_audio',{'path':'memory:999-0','track':'me','at':time.time(),'end':time.time()+.5,'frames':8000,'session':'crash','pcm_base64':base64.b64encode(chunks[0]).decode()})
 wait(lambda:api('/live')['pending']>0);p.kill();p.wait(timeout=10);launch()
 live=api('/live');check('crash reports unfinished RAM loss without arming',live['lost_sources']==1 and live['pending']==0 and live['active']=='false')
 check('completed transcript remains after crash',all(x['kind']=='transcript' and x['text'].strip() for x in live['live_transcription']))
 api('/command',{'command':'quit'});p.wait(timeout=10);check('clean shutdown',p.returncode==0)
 print(json.dumps({'passed':len(checks),'root':str(root)}),flush=True)
finally:
 if 'stop' in globals():stop.set()
 if p is not None and p.poll() is None:p.terminate();p.wait(timeout=10)
