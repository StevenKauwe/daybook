#!/usr/bin/env python3
"""Exercise actual native IPC stalls using synthetic PCM, never capture hardware."""
import json, pathlib, subprocess, time, threading

project=pathlib.Path(__file__).resolve().parents[1]
native=project/'native-rust/target/release/daybook-native'
p=subprocess.Popen([str(native),'--self-test-backpressure'],stdout=subprocess.PIPE,text=True)
timeout=threading.Timer(35,p.kill);timeout.start()
began=time.monotonic();short=False;long=False;audio=[];report=None
try:
    for line in p.stdout:
        event=json.loads(line)
        if event['type']=='audio':
            audio.append({k:v for k,v in event.items() if k!='pcm_base64'})
        elif event['type']=='test':report=event
        elapsed=time.monotonic()-began
        if not short and elapsed>=0.25:
            time.sleep(.8);short=True
        elif not long and elapsed>=3:
            time.sleep(6.3);long=True
    p.wait(timeout=5)
    assert p.returncode==0 and report and short and long
    assert report['fault_events']==0,report
    assert report['peak_packets']>8,report
    assert report['peak_pcm_bytes']<=8*1024*1024,report
    assert report['queue']['queue_pcm_bytes']==0,report
    assert report['queue']['known_rejected_frames']>0,report
    for i,track in enumerate(['me','remote']):
        blocks=[a for a in audio if a['track']==track]
        assert blocks
        # The short pipe stall exceeds the old queue size but loses no audio.
        early=[a for a in blocks if a['at']<report['at']+2.5]
        assert len({a['session'] for a in early})==1
        assert abs(sum(a['frames'] for a in early)/16000-2.5)<.06
        # A stall beyond the bounded cushion reports loss and then recovers.
        assert len({a['session'] for a in blocks})>1
        assert blocks[-1]['end']>=report['at']+11.95
        expected=(report['generated_frames'][i]-report['queue']['losses'][i]['frames'])/report['sample_rates'][i]
        actual=sum(a['frames'] for a in blocks)/16000
        assert abs(expected-actual)<.002,(track,expected,actual)
        for before,after in zip(blocks,blocks[1:]):
            if before['session']==after['session']:
                assert abs(before['end']-after['at'])<.002
            else:
                assert after['at']>before['end']
    print('PASS actual native stdout pipe: 800 ms stall has no loss; 6.3 s stall reports loss and resumes both streams; segment boundaries preserve gaps; all capacity released')
    print('MEASURED',json.dumps(report))
finally:
    timeout.cancel()
    if p.poll() is None:p.terminate();p.wait(timeout=5)
