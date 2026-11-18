#!/usr/bin/env python3
"""Repeatable local overload checks. Never opens microphone or screen capture."""
import os,pathlib,subprocess,sys

root=pathlib.Path(__file__).resolve().parents[1]
evidence=root/'tests/evidence/capture-resilience'
evidence.mkdir(parents=True,exist_ok=True)
env=dict(os.environ,RUSTC_WRAPPER='',CARGO_BUILD_JOBS='1')
commands=[
    ('backend-tests',['cargo','test','--locked']),
    ('native-tests-final',['cargo','test','--locked','--manifest-path','native-rust/Cargo.toml']),
    ('ui-tests',['cargo','test','--locked','--manifest-path','ui-rust/Cargo.toml']),
    ('build-backend',['cargo','build','--release','--locked']),
    ('build-native',['cargo','build','--release','--locked','--manifest-path','native-rust/Cargo.toml']),
    ('pipe-stalls',['python3','tests/capture_resilience.py']),
    ('day-queue',['native-rust/target/release/daybook-native','--self-test-day-queue']),
    ('transcription-pressure',['python3','tests/transcription_backpressure.py']),
    ('disk-bounds',['python3','tests/disk_bounds.py']),
]
for name,cmd in commands:
    path=evidence/(name+'.log')
    with path.open('w') as log:
        result=subprocess.run(cmd,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
    print(f'{name}: '+('PASS' if result.returncode==0 else 'FAILED'),flush=True)
    lines=path.read_text().splitlines()
    for line in lines:
        if line.startswith(('test result:','warning:','error:','error[')):
            print(line[:400],flush=True)
    if result.returncode:
        print('\n'.join(line[:500] for line in lines[-20:]),flush=True)
        sys.exit(result.returncode)
print(f'Passed. Detailed evidence: {evidence}')
print('Synthetic queue/IPC/API tests; not a full-day live recording or battery benchmark.')
