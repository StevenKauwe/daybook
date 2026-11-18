#!/usr/bin/env python3
"""Real Rust menu/window processes with synthetic recording only. No live APIs."""
import json,pathlib,tempfile,subprocess,time,urllib.request,os,signal
project=pathlib.Path(__file__).resolve().parents[1];root=pathlib.Path(tempfile.mkdtemp(prefix='daybook-native-life-'))
bundle=pathlib.Path(os.environ.get('DAYBOOK_TEST_BUNDLE',project/'dist/Daybook.app'))
binary=bundle/'Contents/MacOS/daybook'
p=subprocess.Popen([str(binary),'--synthetic-ui','--data',str(root)],stdout=open(root/'stdout.log','w'),stderr=open(root/'stderr.log','w'))
def eventually(fn):
 for _ in range(150):
  v=fn()
  if v:return v
  time.sleep(.1)
 raise AssertionError('timeout')
try:
 eventually(lambda:(root/'control-url.txt').exists());url=(root/'control-url.txt').read_text()
 def api(path,body=None):
  r=urllib.request.Request(url+path,data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
  return json.load(urllib.request.urlopen(r,timeout=5))
 def command(c):api('/command',{'command':c})
 s=eventually(lambda: (s if s.get('permissions') else None) if (s:=api('/status')) else None)
 helper=json.loads(s['permissions'])['bridge_pid'];assert s['synthetic'] and s['active']=='false'
 def children():return [int(x) for x in subprocess.run(['pgrep','-P',str(helper)],capture_output=True,text=True).stdout.split()]
 ui=eventually(lambda:children())[0]
 command('start');eventually(lambda:api('/status')['active']=='true')
 subprocess.run([str(binary),'--synthetic-ui','--data',str(root)],check=True,timeout=5)
 assert children()==[ui];print('PASS reopening focuses existing window process without second recorder',flush=True)
 os.kill(ui,signal.SIGTERM) # Simulate controls-process exit, not an actual red-close-button click.
 time.sleep(.3);assert p.poll() is None and api('/status')['active']=='true'
 print('PASS controls process exit leaves synthetic recording/menu backend active',flush=True)
 command('open');ui2=eventually(lambda: next((x for x in children() if x!=ui),None))
 command('stop');eventually(lambda:api('/status')['active']=='false')
 c=api('/status')['config'];api('/config',c);time.sleep(.3);assert children()==[ui2]
 print('PASS reopening controls and stopped settings reload preserve recorder ownership',flush=True)
 for cmd in ['permission_mic','permission_screen']:
  command(cmd);eventually(lambda:'suppressed' in api('/status')['notice']);assert api('/status')['active']=='false'
 print('PASS synthetic microphone/screen permission actions stay suppressed and stopped',flush=True)
 command('quit');p.wait(timeout=12);assert p.returncode==0 and not(root/'control-url.txt').exists()
 print('PASS Quit shuts down native helper, controls, and backend',root,flush=True)
finally:
 if p.poll() is None:
  try:command('quit');p.wait(timeout=12)
  except Exception:p.terminate();p.wait(timeout=12)
