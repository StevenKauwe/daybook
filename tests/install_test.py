import importlib.util,pathlib,unittest
spec=importlib.util.spec_from_file_location('installer',pathlib.Path(__file__).resolve().parents[1]/'scripts/install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class InstallTests(unittest.TestCase):
 def test_idempotent_and_preserves_other_apps(self):
  target=pathlib.Path('/Users/example/Applications/Daybook.app');other={'tile-type':'file-tile','tile-data':{'file-label':'Other','custom':42}}
  tiles=m.dock_tiles([other],target);self.assertEqual(tiles[0],other);self.assertEqual(m.dock_tiles(tiles,target),tiles);self.assertEqual(len(tiles),2)
 def test_repoints_old_launcher_and_removes_only_duplicates(self):
  target=pathlib.Path('/Users/example/Applications/Daybook.app');old={'tile-data':{'bundle-identifier':m.BUNDLE_ID,'book':b'stale bookmark','file-mod-date':123,'file-data':{'_CFURLString':'file:///old/Daybook.app/'}}}
  tiles=m.dock_tiles([old,old],target);self.assertEqual(len(tiles),1);self.assertEqual(m.app_url(tiles[0]),target.as_uri()+'/');self.assertNotIn('book',tiles[0]['tile-data']);self.assertNotIn('file-mod-date',tiles[0]['tile-data'])
if __name__=='__main__':unittest.main()
