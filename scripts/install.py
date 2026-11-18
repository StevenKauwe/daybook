#!/usr/bin/env python3
"""Install the built Mac app and a Dock launcher. No capture, login item or permission changes."""
import argparse, pathlib, plistlib, shutil, subprocess, tempfile
ROOT = pathlib.Path(__file__).resolve().parents[1]
BUNDLE_ID = 'local.daybook.recorder'

def read_dock():
    return plistlib.loads(subprocess.check_output(['defaults', 'export', 'com.apple.dock', '-']))

def app_url(tile):
    return tile.get('tile-data', {}).get('file-data', {}).get('_CFURLString', '')

def dock_tiles(tiles, target):
    url = target.as_uri() + '/'
    result=[]; found=False
    for tile in tiles:
        data=tile.get('tile-data', {})
        is_daybook = data.get('bundle-identifier') == BUNDLE_ID or app_url(tile).rstrip('/') == url.rstrip('/')
        if is_daybook:
            if found: continue
            tile=dict(tile); data=dict(data)
            for key in ['book','file-mod-date','parent-mod-date']:data.pop(key,None)
            data['file-data']={'_CFURLString':url,'_CFURLStringType':15}
            data['bundle-identifier']=BUNDLE_ID; data['file-label']='Daybook'; tile['tile-data']=data
            found=True
        result.append(tile)
    if not found:
        result.append({'tile-type':'file-tile','tile-data':{'file-data':{'_CFURLString':url,'_CFURLStringType':15},'file-label':'Daybook','bundle-identifier':BUNDLE_ID}})
    return result

def bundle_id(path):
    with (path/'Contents/Info.plist').open('rb') as f: return plistlib.load(f).get('CFBundleIdentifier')

def ensure_not_running():
    # Installing must not replace executable files under an active recorder.
    processes=subprocess.check_output(['ps','-axo','comm='],text=True).splitlines()
    if any('/Daybook.app/Contents/' in p and p.endswith(('/daybook','/daybook-native','/daybook-ui')) for p in processes):
        raise SystemExit('Quit Daybook from its menu bar before installing. Recording and unsaved controls were left untouched.')

def refresh_dock(target, icon_changed=True):
    # Re-register icon metadata after an atomic bundle replacement. Do not reset global caches.
    subprocess.run(['/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister','-f',str(target)],check=True)
    subprocess.run(['touch',str(target)],check=True)
    dock=read_dock(); old=dock.get('persistent-apps',[]); new=dock_tiles(old,target)
    if new!=old:
        # Only update the application tile array; all other Dock preferences stay intact.
        backup=ROOT/'data/install-backups'; backup.mkdir(parents=True,exist_ok=True)
        with tempfile.NamedTemporaryFile(prefix='dock-',suffix='.plist',dir=backup,delete=False) as f:
            plistlib.dump(dock,f)
        values=[plistlib.dumps(t,fmt=plistlib.FMT_XML).decode() for t in new]
        subprocess.run(['defaults','write','com.apple.dock','persistent-apps','-array',*values],check=True)
    if new!=old or icon_changed:
        subprocess.run(['killall','Dock'],check=False,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)

def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--verify',action='store_true'); parser.add_argument('--check',action='store_true'); parser.add_argument('--refresh-dock',action='store_true'); args=parser.parse_args()
    if args.check: ensure_not_running(); return
    target=pathlib.Path.home()/'Applications/Daybook.app'
    if args.refresh_dock:
        assert bundle_id(target)==BUNDLE_ID
        refresh_dock(target); print('Refreshed Daybook registration and Dock launcher.'); return
    if args.verify:
        assert bundle_id(target)==BUNDLE_ID
        subprocess.run(['codesign','--verify','--deep','--strict',str(target)],check=True)
        matches=[t for t in read_dock().get('persistent-apps',[]) if app_url(t).rstrip('/')==target.as_uri()]
        assert len(matches)==1, 'Expected exactly one installed Dock launcher'
        print('Verified installed Daybook and one Dock launcher.'); return
    source=ROOT/'dist/Daybook.app'
    assert bundle_id(source)==BUNDLE_ID
    subprocess.run(['codesign','--verify','--deep','--strict',str(source)],check=True)
    ensure_not_running()
    icon=pathlib.Path('Contents/Resources/Daybook.icns')
    icon_changed=(source/icon).is_file() and (not (target/icon).is_file() or (source/icon).read_bytes()!=(target/icon).read_bytes())
    if target.exists() and (target.is_symlink() or bundle_id(target)!=BUNDLE_ID):
        raise SystemExit('Existing destination is not the expected Daybook app; refusing to replace it.')
    target.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.daybook-install-',dir=target.parent) as tmp:
        staged=pathlib.Path(tmp)/'Daybook.app';shutil.copytree(source,staged,symlinks=True)
        subprocess.run(['codesign','--verify','--deep','--strict',str(staged)],check=True)
        previous=pathlib.Path(tmp)/'previous.app'
        if target.exists(): target.rename(previous)
        try: staged.rename(target)
        except BaseException:
            if previous.exists():previous.rename(target)
            raise
    refresh_dock(target, icon_changed)
    print(f'Installed {target} and pinned Daybook to the Dock. Recording was not started.')
    print(f'Data and model files remain in {ROOT / "data"}. No login item or privacy permissions were changed.')

if __name__=='__main__':main()
