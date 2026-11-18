#!/usr/bin/env python3
"""Fetch the reviewed GitHub pin into a new isolated snapshot; never touch a working checkout.
No runtime network dependency. Change speechworks-source.json only after reviewing a new commit.
"""
import hashlib, json, os, pathlib, subprocess, tempfile, sys, shutil
root=pathlib.Path(__file__).resolve().parents[1]
lock=json.loads((root/'speechworks-source.json').read_text())
target=root/lock['destination']
if target.parent != root/'vendor' or len(lock['revision']) != 40:
    raise SystemExit('Invalid Speechworks source pin or destination.')
if lock['repository'] != 'https://github.com/StevenKauwe/speechworks.git':
    raise SystemExit('Unexpected Speechworks repository.')
def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()
recovery_patch=root/'patches/reload-model.diff'
if digest(recovery_patch) != lock['recovery_patch_sha256']:
    raise SystemExit('Reviewed Speechworks recovery patch changed.')
def manifest(path):return {str(p.relative_to(path)):digest(p) for p in sorted(path.rglob('*')) if p.is_file() and p.name!='DAYBOOK_SOURCE.json'}
ensure = sys.argv[1:] == ['--ensure']
if sys.argv[1:] == ['--verify'] or (ensure and target.exists()):
    if not (target/'DAYBOOK_SOURCE.json').is_file():
        raise SystemExit(f'No source manifest at {target}. Move aside any incomplete snapshot and run just build with network access.')
    saved=json.loads((target/'DAYBOOK_SOURCE.json').read_text())
    if saved['source'] != lock or saved['files'] != manifest(target):
        raise SystemExit('Speechworks snapshot differs from its source pin or file manifest.')
    print('Verified pinned GitHub snapshot, CPU cap and recovery loader:',lock['revision']);sys.exit(0)
if sys.argv[1:] and not ensure:
    raise SystemExit('Usage: vendor_speechworks.py [--ensure | --verify]')
if target.exists():raise SystemExit('Destination already exists; use --verify, or review a new commit and destination. Never overwrite local changes.')
if os.environ.get('CARGO_NET_OFFLINE', '').lower() == 'true':
    raise SystemExit('Offline build requires the pinned Speechworks snapshot. Run just build once with network access.')
with tempfile.TemporaryDirectory(prefix='daybook-github-source-') as tmp:
    checkout=pathlib.Path(tmp)/'checkout';checkout.mkdir()
    def git(*args):return subprocess.check_output(['git','-C',str(checkout),*args],text=True).strip()
    git('init','--quiet');git('fetch','--quiet','--depth=1',lock['repository'],lock['revision'])
    if git('rev-parse','FETCH_HEAD') != lock['revision'] or git('rev-parse','FETCH_HEAD^{tree}') != lock['tree']:
        raise SystemExit('Fetched Speechworks revision or tree differs from the reviewed pin.')
    archive=pathlib.Path(tmp)/'source.tar';git('archive','--format=tar','--output='+str(archive),'FETCH_HEAD')
    stage=pathlib.Path(tmp)/'snapshot';stage.mkdir();subprocess.run(['tar','-xf',str(archive),'-C',str(stage)],check=True)
    cpu=stage/'crates/inference/src/cpu/mod.rs';text=cpu.read_text()
    old='std::thread::available_parallelism().map_or(1, |n| n.get())'
    if text.count(old) != 1:
        raise SystemExit('Upstream thread implementation changed. Review the CPU cap patch.')
    text=text.replace(old,f'std::thread::available_parallelism().map_or(1, |n| n.get().min({lock["cpu_threads"]}))')
    cpu.write_text(text)
    subprocess.run(['patch','-p1','-i',str(recovery_patch)],cwd=stage,check=True)
    (stage/'DAYBOOK_SOURCE.json').write_text(json.dumps({'source':lock,'patch':'Two-thread CPU cap and optional recovery loader for releasing the original GGUF after load.','files':manifest(stage)},indent=2)+'\n')
    # Copy to a temporary sibling, then rename; existing destinations are never replaced.
    target.parent.mkdir(exist_ok=True)
    pending=pathlib.Path(tempfile.mkdtemp(prefix='.speechworks-',dir=target.parent))
    try:
        shutil.copytree(stage,pending,dirs_exist_ok=True)
        if target.exists():raise RuntimeError('destination appeared during fetch')
        pending.rename(target)
    finally:
        if pending.exists():shutil.rmtree(pending)
print('Vendored reviewed GitHub source:',lock['revision'])
