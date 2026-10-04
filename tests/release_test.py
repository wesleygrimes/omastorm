"""Staged release regressions: fake public assets/endpoints, isolated Git repos."""
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts/tooling'))
import release


def elf(machine):
    data = bytearray(128)
    data[:7] = b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HHI', data, 16, 2, machine, 1)
    struct.pack_into('<QQ', data, 24, 0x400078, 64)
    struct.pack_into('<HHH', data, 52, 64, 56, 1)
    struct.pack_into('<IIQQQQQQ', data, 64, 1, 5, 0, 0x400000, 0, 128, 128, 4096)
    return bytes(data)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / 'engine').mkdir()
        (self.root / 'engine/Cargo.toml').write_text('[package]\nname = "omastorm-engine"\nversion = "1.2.3"\n')
        (self.root / 'Cargo.lock').write_text('version = 4\n\n[[package]]\nname = "omastorm-engine"\nversion = "1.2.3"\n\n[[package]]\nname = "dependency"\nversion = "9.0.0"\n')
        (self.root / 'manifest.json').write_text('{"id":"com.omastorm.radar", "version":"1.2.3"}\n')
        (self.root / 'engine/release.pin').write_text('tag=engine-1.2.3\nrepo=fixture/omastorm\n')
        self.originals = {p: p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        self.root_patch = patch.object(release, 'ROOT', self.root)
        self.root_patch.start()
        self.pin_patch = patch.object(release, 'pin', return_value={'repo':'fixture/omastorm','tag':'engine-1.2.3'})
        self.pin_patch.start()
        self.git('init','-q','-b','topic')
        self.git('config','user.name','Fixture')
        self.git('config','user.email','fixture@example.invalid')
        self.git('add','.')
        self.git('-c','commit.gpgsign=false','commit','-qm','fixture')

    def tearDown(self):
        self.pin_patch.stop()
        self.root_patch.stop()
        self.tmp.cleanup()

    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.root),*args],stderr=subprocess.PIPE).decode().strip()

    def test_prepare_updates_only_package_versions_and_never_commits(self):
        head = self.git('rev-parse','HEAD')
        release.prepare('engine','1.2.4')
        self.assertIn('version = "1.2.4"', (self.root / 'engine/Cargo.toml').read_text())
        self.assertIn('name = "dependency"\nversion = "9.0.0"', (self.root / 'Cargo.lock').read_text())
        self.assertEqual(self.git('rev-parse','HEAD'), head)
        with self.assertRaisesRegex(RuntimeError,'already have changes'):
            release.prepare('engine','1.2.5')

    def test_file_edits_reject_main_detached_dirty_versions_atomically(self):
        for branch in ('main','detached'):
            self.git('switch','-q','-C','main') if branch=='main' else self.git('checkout','-q','--detach')
            for operation in (lambda:release.prepare('engine','1.2.4'),lambda:release.prepare('plugin','1.2.4'),lambda:release.write_pin('engine-1.2.4')):
                with self.assertRaises(RuntimeError): operation()
            self.git('switch','-q','topic')
        for version in ('bogus','01.2.4','1.2.3','1.2.2'):
            with self.assertRaises((ValueError,RuntimeError)): release.prepare('plugin',version)
        for path, original in self.originals.items(): self.assertEqual(path.read_bytes(),original)
        (self.root/'Cargo.lock').write_text((self.root/'Cargo.lock').read_text().replace('version = "1.2.3"','version = "1.2.2"'))
        self.git('add','Cargo.lock'); self.git('-c','commit.gpgsign=false','commit','-qm','stale')
        with self.assertRaisesRegex(RuntimeError,'already match'): release.prepare('engine','1.2.4')

    def assets(self):
        tag='engine-1.2.4'; source='a'*40; files={}; pin={'tag':tag,'repo':'fixture/omastorm'}; sums=[]
        for arch, machine in release.ARCHES.items():
            name=f'omastorm-engine-{arch}-unknown-linux-gnu'
            payload=elf(machine)
            checksum=hashlib.sha256(payload).hexdigest()
            files[name]=payload
            files[name+'.build.json']=json.dumps(dict(source=source,version='1.2.4',asset=name,sha256=checksum)).encode()
            pin.update({f'asset_{arch}':name,f'sha256_{arch}':checksum})
            sums.append(f'{checksum}  {name}\n')
        files['release.pin']=''.join(f'{k}={v}\n' for k,v in pin.items()).encode()
        files['SHA256SUMS']=''.join(sums).encode()
        info=dict(draft=False,published_at='2026-10-01T12:00:00Z',tag_name=tag,assets=[dict(name=name,state='uploaded',browser_download_url=f'https://github.com/fixture/omastorm/releases/download/{tag}/{name}') for name in files])
        return files,info

    def public(self, files, info):
        def endpoint(path):
            if '/releases/tags/' in path: return info
            return {'object':{'type':'commit','sha':'a'*40}}
        def download(url,path): path.write_bytes(files[url.rsplit('/',1)[1]])
        return patch.object(release,'gh',side_effect=endpoint),patch.object(release,'download',side_effect=download)

    def test_public_assets_pin_atomically_and_fail_closed(self):
        for failure in ('draft','unpublished','missing-arm','checksum','architecture','source','version','pin','malformed-sums','duplicate-pin','download','truncated-elf','bad-load','no-executable-entry'):
            with self.subTest(failure=failure):
                files,info=self.assets(); arm='omastorm-engine-aarch64-unknown-linux-gnu'
                if failure=='draft':info['draft']=True
                elif failure=='unpublished':info['published_at']=None
                elif failure=='missing-arm':info['assets']=[a for a in info['assets'] if a['name']!=arm]
                elif failure=='checksum':files[arm]+=b'corrupt'
                elif failure=='architecture':files[arm]=elf(62)
                elif failure in ('source','version'):
                    meta=json.loads(files[arm+'.build.json']);meta[failure]='wrong';files[arm+'.build.json']=json.dumps(meta).encode()
                elif failure=='pin':files['release.pin']+=b'unknown=value\n'
                elif failure=='malformed-sums':files['SHA256SUMS']=b'wrong\n'
                elif failure=='duplicate-pin':files['release.pin']+=b'tag=engine-1.2.4\n'
                elif failure=='download':files.pop(arm)
                elif failure in ('truncated-elf','bad-load','no-executable-entry'):
                    payload=bytearray(files[arm])
                    if failure=='truncated-elf':payload=payload[:20]
                    elif failure=='bad-load':struct.pack_into('<Q',payload,96,1024)
                    else:struct.pack_into('<I',payload,68,4)
                    old=hashlib.sha256(files[arm]).hexdigest(); new=hashlib.sha256(payload).hexdigest()
                    files[arm]=bytes(payload)
                    for file in (arm+'.build.json','release.pin','SHA256SUMS'):files[file]=files[file].replace(old.encode(),new.encode())
                api,download=self.public(files,info)
                before=(self.root/'engine/release.pin').read_bytes()
                with api,download,self.assertRaises((RuntimeError,KeyError)):
                    release.write_pin('engine-1.2.4')
                self.assertEqual((self.root/'engine/release.pin').read_bytes(),before)
        files,info=self.assets(); api,download=self.public(files,info)
        with api,download:release.write_pin('engine-1.2.4')
        self.assertIn('tag=engine-1.2.4',(self.root/'engine/release.pin').read_text())
        self.assertEqual(self.git('rev-parse','HEAD'),self.git('rev-parse','topic'))

    def test_tag_guards_and_family_notes(self):
        calls=[]
        def guard(*args):
            if args[:2]==('symbolic-ref','--quiet'):return branch[0]
            if args==('status','--porcelain'):return dirty[0]
            if args==('rev-parse','HEAD'):return 'a'*40
            if args[:2]==('ls-remote','origin') and args[-1]=='refs/heads/main':return remote[0]+'\trefs/heads/main'
            if args==('ls-remote','--tags','origin'):return ('b'*40+'\trefs/tags/engine-1.2.2\n'+'c'*40+'\trefs/tags/v1.2.2')
            if args[:2]==('ls-remote','origin'):return existing[0]
            if args[:2]==('tag','--list'):return existing[0] if args[2]=='v1.2.3' else 'engine-1.2.2\nv1.2.2' if args[2].startswith('v') else 'engine-1.2.2'
            if args[:2]==('remote','get-url'):return 'https://github.com/fixture/omastorm.git'
            raise AssertionError(args)
        branch=['topic'];dirty=[''];remote=['a'*40];existing=['']
        absent=subprocess.CalledProcessError(1,['gh'],stderr='HTTP 404')
        def endpoint(path,*args):
            if path=='graphql':return {'data':{'repository':{'release':None}}}
            if '/compare/' in path:return {'status':'ahead'}
            if path.endswith('/generate-notes'):return {'body':'Changes since v1.2.2'}
            raise absent
        with patch.object(release,'git',side_effect=guard),patch.object(release,'gh',side_effect=endpoint),patch.object(release,'run',side_effect=lambda *a,**k:calls.append(a)):
            with self.assertRaisesRegex(RuntimeError,'main'):release.tag('plugin')
            branch[0]='main';dirty[0]='dirty'
            with self.assertRaisesRegex(RuntimeError,'clean'):release.tag('plugin')
            dirty[0]='';remote[0]='b'*40
            with self.assertRaisesRegex(RuntimeError,'remote main'):release.tag('plugin')
            remote[0]='a'*40;existing[0]='v1.2.3'
            with self.assertRaisesRegex(RuntimeError,'Existing immutable'):release.tag('plugin')
            self.assertFalse(calls)
            existing[0]=''
            release.tag('plugin')
        draft=calls[-1]
        self.assertIn('--draft',draft)
        self.assertEqual(draft[draft.index('--title')+1],'Omastorm 1.2.3')
        self.assertIn('Changes since v1.2.2',draft)
        self.assertNotIn('engine-1.2.2',draft)
        self.assertFalse(any('--clobber' in call or '--latest' in call for call in calls))

    def test_origin_and_remote_notes_fail_closed(self):
        for url in ('https://github.com/fork/omastorm.git','/tmp/local.git'):
            with patch.object(release,'git',return_value=url),self.assertRaisesRegex(RuntimeError,'Origin'):
                release.origin_repo()
        for url in ('https://github.com/fixture/omastorm.git','git@github.com:fixture/omastorm.git','ssh://git@github.com/fixture/omastorm.git'):
            with patch.object(release,'git',return_value=url):self.assertEqual(release.origin_repo(),'fixture/omastorm')
        tags='\n'.join('b'*40+'\trefs/tags/'+tag for tag in ('engine-1.2.2','v1.2.1','v1.2.2','v9.0.0'))
        seen=[]
        def endpoint(path,*args):
            seen.append((path,args))
            if '/compare/' in path:return {'status':'diverged'}
            raise AssertionError(path)
        with patch.object(release,'git',return_value=tags),patch.object(release,'gh',side_effect=endpoint):
            notes=release.release_notes('plugin','v1.2.3','fixture/omastorm','a'*40)
        self.assertIn('Initial plugin release',notes)
        self.assertEqual(len(seen),2)  # only older tags in the same family
        with patch.object(release,'git',side_effect=['https://github.com/fixture/omastorm.git','https://github.com/fork/omastorm.git']),self.assertRaisesRegex(RuntimeError,'push destination'):
            release.origin_repo()

    def test_notes_keep_only_what_each_product_ships(self):
        url='https://github.com/fixture/omastorm/pull/'
        body='\n'.join(['<!-- generated -->','','## What\'s Changed','### Fixes',
            f'* fix(engine): cap bodies by @a in {url}1',f'* fix(ui): chip clicks by @b in {url}2',
            '### Features',f'* feat(engine): fallback (#4) by @a in {url}3',f'* revert(engine): drop the fallback (#3) by @a in {url}4',
            '### Other changes',f'* refactor: tooling by @a in {url}5',f'* chore: pin engine-1.2.3 by @a in {url}6','',
            '## New Contributors',f'* @b made their first contribution in {url}2','',
            '**Full Changelog**: https://github.com/fixture/omastorm/compare/a...b'])
        files={1:['engine/src/live.rs'],2:['ui/Popover.qml','engine/README.md'],3:['engine/src/eccc.rs'],
               4:['engine/src/eccc.rs'],5:['scripts/tooling/cli.py','engine/tests/protocol.rs'],6:['engine/release.pin']}
        def endpoint(*args):
            return [[{'filename':name} for name in files[int(args[-1].split('/')[-2])]]]
        with patch.object(release,'gh',side_effect=endpoint):
            engine=release.product_notes('engine',body,'fixture/omastorm')
            plugin=release.product_notes('plugin',body,'fixture/omastorm')
        self.assertIn(url+'1',engine)
        for absent in (url+'2',url+'3',url+'4',url+'5',url+'6','### Features','### Other changes','New Contributors'):
            self.assertNotIn(absent,engine)
        self.assertIn('### Fixes',engine);self.assertIn('**Full Changelog**',engine)
        for present in (url+'2',url+'6','## New Contributors','@b made their first'):
            self.assertIn(present,plugin)
        for absent in (url+'1',url+'3',url+'4',url+'5','### Features'):
            self.assertNotIn(absent,plugin)
        self.assertNotIn('\n\n\n',engine+plugin)

    def test_draft_refuses_existing_draft_and_published_releases_before_mutation(self):
        for draft in (True,False):
            def endpoint(path,*args):
                if path=='graphql':return {'data':{'repository':{'release':{'id':'fixture','isDraft':draft}}}}
                raise subprocess.CalledProcessError(1,['gh'],stderr='HTTP 404')
            with patch.object(release,'origin_repo',return_value='fixture/omastorm'),patch.object(release,'gh',side_effect=endpoint),patch.object(release,'run') as mutation:
                with self.assertRaisesRegex(RuntimeError,'Existing release'):release.draft_engine(self.root/'dist','engine-1.2.3')
                mutation.assert_not_called()

    def test_packaging_preserves_both_architectures_source_hash_and_tracked_pin(self):
        dist=self.root/'dist';dist.mkdir()
        source=self.git('rev-parse','HEAD')
        for arch,machine in release.ARCHES.items():
            name=f'omastorm-engine-{arch}-unknown-linux-gnu'
            payload=elf(machine);(dist/name).write_bytes(payload)
            (dist/(name+'.build.json')).write_text(json.dumps(dict(source=source,version='1.2.3',asset=name,sha256=hashlib.sha256(payload).hexdigest())))
        before=(self.root/'engine/release.pin').read_bytes()
        release.package(dist)
        self.assertEqual(len((dist/'SHA256SUMS').read_text().splitlines()),2)
        self.assertEqual((self.root/'engine/release.pin').read_bytes(),before)
        arm=dist/'omastorm-engine-aarch64-unknown-linux-gnu'
        original=arm.read_bytes();metadata=arm.with_name(arm.name+'.build.json');saved=metadata.read_bytes()
        for failure in ('missing','architecture','source','version','hash'):
            with self.subTest(failure=failure):
                if failure=='missing':arm.unlink()
                elif failure=='architecture':arm.write_bytes(elf(62))
                else:
                    values=json.loads(saved);values[{'hash':'sha256'}.get(failure,failure)]='wrong';metadata.write_text(json.dumps(values))
                with self.assertRaises((RuntimeError,FileNotFoundError)):release.package(dist)
                arm.write_bytes(original);metadata.write_bytes(saved)
        self.assertEqual((self.root/'engine/release.pin').read_bytes(),before)


if __name__=='__main__': unittest.main()
