"""PR-only version/pin edits; immutable tags and explicitly drafted releases."""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import socket
import tempfile
import tomllib

from runtime import ROOT, pin, run, Process, wait_until

ARCHES = {'x86_64': 62, 'aarch64': 183}
VERSION = r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'


def git(*args):
    return run('git', *args, cwd=ROOT, capture_output=True, text=True).stdout.strip()


def gh(*args):
    return json.loads(run('gh', 'api', *args, capture_output=True, text=True).stdout)


def working_branch(paths):
    try:
        branch = git('symbolic-ref', '--quiet', '--short', 'HEAD')
    except subprocess.CalledProcessError:
        raise RuntimeError('Release edits require a working branch, not detached HEAD.') from None
    if branch == 'main':
        raise RuntimeError('Release edits require a working branch and a PR; main cannot be edited.')
    if git('diff', '--name-only', 'HEAD', '--', *paths):
        raise RuntimeError('Release files already have changes; preserve/review them before preparing another release.')
    for path in paths:
        if (ROOT / path).is_symlink():
            raise RuntimeError(f'Refusing symlink release file: {path}')


def atomic_write(path, contents):
    path = Path(path)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix='.' + path.name + '.', delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        except BaseException:
            temporary.unlink()
            raise
    try:
        os.chmod(temporary, path.stat().st_mode & 0o777)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def prepare(product, version):
    paths = ['manifest.json'] if product == 'plugin' else ['engine/Cargo.toml', 'Cargo.lock']
    working_branch(paths)
    if not re.fullmatch(VERSION, version):
        raise ValueError('Version must be X.Y.Z without leading zeroes.')
    originals = {path: (ROOT / path).read_text() for path in paths}
    if product == 'plugin':
        manifest = json.loads(originals['manifest.json'])
        current = manifest['version']
        manifest['version'] = version
        updates = {'manifest.json': json.dumps(manifest, indent=2) + '\n'}
    else:
        package = tomllib.loads(originals['engine/Cargo.toml'])['package']
        current, name = package['version'], package['name']
        locked = [p for p in tomllib.loads(originals['Cargo.lock'])['package'] if p['name'] == name]
        if len(locked) != 1 or locked[0]['version'] != current:
            raise RuntimeError('Cargo.lock must already match the engine version.')
        toml, count = re.subn(r'^version = "' + re.escape(current) + r'"$', f'version = "{version}"', originals['engine/Cargo.toml'], count=1, flags=re.M)
        pattern = r'(\[\[package\]\]\nname = "' + re.escape(name) + r'"\nversion = ")' + re.escape(current) + r'(")'
        lock, changed = re.subn(pattern, lambda m: m[1] + version + m[2], originals['Cargo.lock'])
        if count != 1 or changed != 1:
            raise RuntimeError('Expected exactly one engine package version in manifest and lockfile.')
        updates = {'engine/Cargo.toml': toml, 'Cargo.lock': lock}
        if version == pin()['tag'].removeprefix('engine-'):
            raise RuntimeError('That engine version is already the published pin.')
    if tuple(map(int, version.split('.'))) <= tuple(map(int, current.split('.'))):
        raise RuntimeError(f'New version must be greater than {current}.')
    written = []
    try:
        for path, contents in updates.items():
            atomic_write(ROOT / path, contents)
            written.append(path)
    except BaseException:
        for path in written:
            atomic_write(ROOT / path, originals[path])
        raise
    print(f'Prepared {product} {version} locally. Review and submit a PR; no commit or push performed.')


def release_source(repo, tag):
    obj = gh(f'repos/{repo}/git/ref/tags/{tag}')['object']
    for _ in range(5):
        if obj['type'] == 'commit' and re.fullmatch('[a-f0-9]{40}', obj['sha']):
            return obj['sha']
        if obj['type'] != 'tag':
            break
        obj = gh(f'repos/{repo}/git/tags/{obj["sha"]}')['object']
    raise RuntimeError('Release tag does not resolve to an immutable source commit.')


def verify_elf(path, arch):
    with Path(path).open('rb') as source:
        size = os.fstat(source.fileno()).st_size
        header = source.read(64)
        if len(header) != 64:
            raise RuntimeError(f'Truncated ELF header: {path}')
        ident, kind, machine, version, entry, offset, _, _, header_size, entry_size, count, _, _, _ = struct.unpack('<16sHHIQQQIHHHHHH', header)
        if ident[:7] != b'\x7fELF\x02\x01\x01' or machine != ARCHES[arch]:
            raise RuntimeError(f'Wrong ELF architecture for {arch}: {path}')
        if kind not in (2, 3) or version != 1 or header_size != 64 or entry_size != 56 or not count or offset < 64 or offset + count * entry_size > size:
            raise RuntimeError(f'Malformed executable ELF: {path}')
        source.seek(offset)
        executable_entry = False
        for _ in range(count):
            segment, flags, start, address, _, file_size, memory_size, alignment = struct.unpack('<IIQQQQQQ', source.read(56))
            if segment != 1:  # PT_LOAD
                continue
            if file_size > memory_size or start + file_size > size or (alignment > 1 and (alignment & (alignment - 1) or start % alignment != address % alignment)):
                raise RuntimeError(f'Malformed ELF load segment: {path}')
            executable_entry |= bool(flags & 1 and address <= entry < address + file_size)
        if not executable_entry:
            raise RuntimeError(f'ELF entry point is not backed by executable bytes: {path}')


def download(url, dest):
    run('curl', '-fsSL', '--max-time', '90', '-o', dest, '--', url, timeout=100)


def verified_pin(tag):
    if not re.fullmatch('engine-' + VERSION, tag):
        raise ValueError('Tag must be engine-X.Y.Z.')
    repo = pin()['repo']
    release = gh(f'repos/{repo}/releases/tags/{tag}')
    if release.get('draft') is not False or not release.get('published_at') or release.get('tag_name') != tag:
        raise RuntimeError('Pin requires a public published release, never a draft.')
    source = release_source(repo, tag)
    names = [f'omastorm-engine-{arch}-unknown-linux-gnu' for arch in ARCHES]
    required = names + [name + '.build.json' for name in names] + ['SHA256SUMS', 'release.pin']
    assets = release.get('assets', [])
    if len({asset['name'] for asset in assets}) != len(assets):
        raise RuntimeError('Duplicate release asset names.')
    assets = {asset['name']: asset for asset in assets}
    if set(required) - assets.keys():
        raise RuntimeError('Missing release assets: ' + ', '.join(sorted(set(required) - assets.keys())))
    with tempfile.TemporaryDirectory(prefix='omastorm-pin-') as scratch:
        root = Path(scratch)
        for name in required:
            url = assets[name]['browser_download_url']
            expected = f'https://github.com/{repo}/releases/download/{tag}/{name}'
            if url != expected or assets[name].get('state', 'uploaded') != 'uploaded':
                raise RuntimeError(f'Unexpected public asset URL/state: {name}')
            download(url, root / name)
        sums = {}
        for line in (root / 'SHA256SUMS').read_text().splitlines():
            match = re.fullmatch(r'([a-f0-9]{64})  (omastorm-engine-(?:x86_64|aarch64)-unknown-linux-gnu)', line)
            if not match or match[2] in sums:
                raise RuntimeError('Malformed or duplicate package checksum entries.')
            sums[match[2]] = match[1]
        if set(sums) != set(names):
            raise RuntimeError('Package checksums must cover both architectures exactly.')
        candidate = {}
        for line in (root / 'release.pin').read_text().splitlines():
            if not line or line.startswith('#'):
                continue
            key, separator, value = line.partition('=')
            if not separator or key in candidate:
                raise RuntimeError('Malformed or duplicate candidate pin entries.')
            candidate[key] = value
        expected = {'tag':tag, 'repo':repo}
        for arch, name in zip(ARCHES, names):
            verify_elf(root / name, arch)
            checksum = hashlib.sha256((root / name).read_bytes()).hexdigest()
            metadata = json.loads((root / (name + '.build.json')).read_text())
            if checksum != sums[name] or metadata != {'source':source, 'version':tag.removeprefix('engine-'), 'asset':name, 'sha256':checksum}:
                raise RuntimeError(f'Checksum/source/version metadata mismatch for {name}.')
            expected.update({f'asset_{arch}':name, f'sha256_{arch}':checksum})
        if candidate != expected:
            raise RuntimeError('Candidate pin does not exactly match the verified public package.')
        return '# Verified public engine release; update through mise release engine pin.\n' + ''.join(f'{key}={value}\n' for key,value in expected.items())


def write_pin(tag):
    working_branch(['engine/release.pin'])
    contents = verified_pin(tag)
    # Revalidate local edits after the bounded network verification.
    working_branch(['engine/release.pin'])
    atomic_write(ROOT / 'engine/release.pin', contents)
    print('Verified both public architectures and wrote the pin locally. Submit a compatible UI/pin PR; no commit/push performed.')


def origin_repo():
    url = git('remote', 'get-url', 'origin')
    urls = [url, *git('remote', 'get-url', '--push', '--all', 'origin').splitlines()]
    for destination in urls:
        match = re.fullmatch(r'(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)([\w.-]+/[\w.-]+?)(?:\.git)?/?', destination)
        if not match or match[1].lower() != pin()['repo'].lower():
            raise RuntimeError('Origin and every push destination must match the GitHub release repository in the committed pin.')
    return pin()['repo']


def require_no_release(repo, name):
    owner, repository = repo.split('/',1)
    # REST's tag lookup misses drafts whose tag is still pending.
    query = 'query($owner:String!,$name:String!,$tag:String!){repository(owner:$owner,name:$name){release(tagName:$tag){id isDraft}}}'
    response = gh('graphql', '-f', 'query=' + query, '-f', 'owner=' + owner, '-f', 'name=' + repository, '-f', 'tag=' + name)
    if response.get('errors') or not response.get('data', {}).get('repository'):
        raise RuntimeError('Could not verify release absence in the release repository.')
    if response['data']['repository']['release'] is not None:
        raise RuntimeError(f'Existing release {name}; refusing replacement.')


def previous_tag(product, current, repo, head):
    prefix = 'engine-' if product == 'engine' else 'v'
    current_version = tuple(map(int, current.removeprefix(prefix).split('.')))
    tags = {}
    # Fresh remote state also works in shallow clones with no local tags.
    for line in git('ls-remote', '--tags', 'origin').splitlines():
        sha, ref = line.split()
        name = ref.removeprefix('refs/tags/').removesuffix('^{}')
        if re.fullmatch(prefix + VERSION, name):
            if not name in tags or ref.endswith('^{}'):
                tags[name] = sha
    ordered = sorted(tags, key=lambda name: tuple(map(int, name.removeprefix(prefix).split('.'))), reverse=True)
    for name in ordered:
        if tuple(map(int, name.removeprefix(prefix).split('.'))) < current_version:
            comparison = gh(f'repos/{repo}/compare/{tags[name]}...{head}')
            if comparison['status'] in ('ahead', 'identical'):
                return name
    return None


# The paths behind what each product ships; a PR appears in that product's
# notes only when it changed one. Engine tests and docs ship nothing.
def ships(product, path):
    if product == 'engine':
        return (path.startswith('engine/') and not path.startswith('engine/tests/')
                and not path.endswith('.md') and path != 'engine/release.pin') or path in ('Cargo.toml', 'Cargo.lock')
    return path.startswith(('ui/', 'scripts/hooks/')) or path in (
        'manifest.json', 'run.sh', 'engine/release.pin',
        'scripts/fetch-engine.sh', 'scripts/engine-pin.sh', 'scripts/write-desktop-entry.sh')


ENTRY = re.compile(r'^\* (.*) by @\S+ in https://github\.com/\S+/pull/(\d+)$')
CONTRIBUTOR = re.compile(r'^\* @\S+ made their first contribution in https://github\.com/\S+/pull/(\d+)$')


def product_notes(product, body, repo):
    # GitHub lists every PR between two tags. Keep those that changed what this
    # product ships, less any PR reverted in the same range and its revert.
    lines = body.split('\n')
    titles = {int(m[2]): m[1] for line in lines if (m := ENTRY.match(line))}
    keep = {n for n in titles if any(ships(product, f['filename'])
            for page in gh('--paginate', '--slurp', f'repos/{repo}/pulls/{n}/files') for f in page)}
    for n, pr_title in titles.items():
        reverted = re.search(r'\(#(\d+)\)', pr_title)
        if pr_title.lower().startswith('revert') and reverted and int(reverted[1]) in titles:
            keep -= {n, int(reverted[1])}
    lines = [line for line in lines if not (m := ENTRY.match(line) or CONTRIBUTOR.match(line)) or int(m[m.lastindex]) in keep]
    out = []
    for i, line in enumerate(lines):
        level = len(line) - len(line.lstrip('#'))
        if level >= 2:
            end = next((j for j in range(i + 1, len(lines)) if lines[j].startswith('**Full Changelog')
                        or 0 < len(lines[j]) - len(lines[j].lstrip('#')) <= level), len(lines))
            if not any(entry.startswith('* ') for entry in lines[i + 1:end]):
                continue
        out.append(line)
    return re.sub(r'\n{3,}', '\n\n', '\n'.join(out))


def release_notes(product, name, repo, head):
    previous = previous_tag(product, name, repo, head)
    if previous:
        body = gh(f'repos/{repo}/releases/generate-notes', '-f', f'tag_name={name}', '-f', f'target_commitish={head}', '-f', f'previous_tag_name={previous}')['body']
        return product_notes(product, body, repo)
    # Never let GitHub implicitly compare the first tag with another family.
    return f'Initial {product} release {name}.\n\nSource commit: {head}\n'


def title(product, version):
    # Tags carry the v/engine- prefix; release titles name the product.
    return ('Omastorm ' if product == 'plugin' else 'Engine ') + version


def tag(product):
    if git('symbolic-ref', '--quiet', '--short', 'HEAD') != 'main':
        raise RuntimeError('Tags require clean current main, after the version PR merges.')
    if git('status', '--porcelain'):
        raise RuntimeError('Tagging requires a clean worktree.')
    repo = origin_repo()
    head = git('rev-parse', 'HEAD')
    remote = git('ls-remote', 'origin', 'refs/heads/main').split()
    if not remote or remote[0] != head:
        raise RuntimeError('Local main does not match freshly verified remote main.')
    version = (tomllib.loads((ROOT / 'engine/Cargo.toml').read_text())['package']['version'] if product == 'engine'
               else json.loads((ROOT / 'manifest.json').read_text())['version'])
    if not re.fullmatch(VERSION, version):
        raise RuntimeError('Manifest version must be X.Y.Z.')
    name = ('engine-' if product == 'engine' else 'v') + version
    if git('tag', '--list', name) or git('ls-remote', 'origin', f'refs/tags/{name}'):
        raise RuntimeError(f'Existing immutable tag {name}; refusing replacement.')
    require_no_release(repo, name)
    notes = release_notes(product, name, repo, head) if product == 'plugin' else None
    # The remote can advance while release lookup/notes run; check it again
    # immediately before tag creation/push.
    if git('ls-remote', 'origin', 'refs/heads/main').split()[0] != head or git('status', '--porcelain'):
        raise RuntimeError('Main/worktree changed while validating the release.')
    run('git', 'tag', '-a', name, head, '-m', name, cwd=ROOT)
    run('git', 'push', 'origin', f'refs/tags/{name}', cwd=ROOT, timeout=120)
    if product == 'plugin':
        args = ['gh', 'release', 'create', name, '--repo', repo, '--verify-tag', '--draft', '--notes', notes, '--title', title('plugin', version)]
        run(*args, timeout=120)
    print(f'Tagged {name}. {"CI will draft native engine assets" if product == "engine" else "Plugin release is a draft"}; publication is an explicit GitHub action.')


def execute(args):
    if args.stage == 'prepare':
        prepare(args.product, args.version)
    elif args.stage == 'pin':
        write_pin(args.tag)
    else:
        tag(args.product)


def package(dist):
    """Internal native CI operation: validate both builds before writing a bundle."""
    dist = Path(dist)
    source = git('rev-parse', 'HEAD')
    version = tomllib.loads((ROOT / 'engine/Cargo.toml').read_text())['package']['version']
    values = {'tag': 'engine-' + version, 'repo': pin()['repo']}
    sums = []
    for arch in ARCHES:
        name = f'omastorm-engine-{arch}-unknown-linux-gnu'
        path = dist / name
        verify_elf(path, arch)
        checksum = hashlib.sha256(path.read_bytes()).hexdigest()
        metadata = json.loads((dist / (name + '.build.json')).read_text())
        if metadata != {'source':source, 'version':version, 'asset':name, 'sha256':checksum}:
            raise RuntimeError(f'Build metadata does not match {name}, engine {version}, and commit {source}')
        values.update({f'asset_{arch}':name, f'sha256_{arch}':checksum})
        sums.append(f'{checksum}  {name}\n')
    (dist / 'release.pin').write_text('# Verify published assets with mise release engine pin before committing.\n' + ''.join(f'{k}={v}\n' for k,v in values.items()))
    (dist / 'SHA256SUMS').write_text(''.join(sums))
    for arch in ARCHES:
        (dist / values['asset_' + arch]).chmod(0o755)
    print(f'Validated native release bundle: {dist}; tracked pin unchanged.')


def smoke(path):
    """Native execution validates the real wire hello, beyond ELF structure."""
    from suite import providers, isolated_environment
    path = Path(path).resolve()
    version = tomllib.loads((ROOT / 'engine/Cargo.toml').read_text())['package']['version']
    protocol = int(re.search(r'^pub const VERSION: u32 = (\d+);$', (ROOT / 'engine/src/protocol.rs').read_text(), re.M)[1])
    (ROOT / 'target/evidence').mkdir(parents=True, exist_ok=True)
    with providers() as tiles, tempfile.TemporaryDirectory(prefix='omastorm-smoke-', dir='/tmp') as scratch:
        env = isolated_environment(Path(scratch), tiles)
        daemon = Process([path, 'serve'], env, ROOT / 'target/evidence/native-smoke.log')
        try:
            address = Path(env['XDG_RUNTIME_DIR']) / 'omastorm/engine.sock'
            wait_until(address.exists, process=daemon.child)
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(3)
                client.connect(str(address))
                with client.makefile('rb') as stream:
                    line = stream.readline(1024 * 1024)
                    if not line.endswith(b'\n'): raise RuntimeError('Missing or oversized native hello.')
                    hello = json.loads(line)
            if hello.get('type') != 'hello' or hello.get('engine') != version or hello.get('v') != protocol:
                raise RuntimeError(f'Candidate hello does not match engine {version}/protocol {protocol}: {hello}')
        finally:
            daemon.close()
    print(f'Native candidate engine {version}/protocol {protocol}: PASS')


def draft_engine(dist, name):
    repo = origin_repo()
    require_no_release(repo, name)
    head = git('rev-parse', 'HEAD')
    version = tomllib.loads((ROOT / 'engine/Cargo.toml').read_text())['package']['version']
    if name != 'engine-' + version or release_source(repo, name) != head:
        raise RuntimeError('Engine release tag/source/version mismatch.')
    package(dist)
    notes = release_notes('engine', name, repo, head)
    assets = [str(Path(dist) / asset) for asset in [f'omastorm-engine-{a}-unknown-linux-gnu{suffix}' for a in ARCHES for suffix in ('', '.build.json')] + ['SHA256SUMS', 'release.pin']]
    run('gh', 'release', 'create', name, '--repo', repo, '--verify-tag', '--draft', '--title', title('engine', version), '--notes', notes, *assets, timeout=120)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='Internal native CI operations; public release stages use mise release.')
    commands = parser.add_subparsers(dest='operation', required=True)
    commands.add_parser('package').add_argument('dist')
    commands.add_parser('smoke').add_argument('binary')
    draft = commands.add_parser('draft-engine')
    draft.add_argument('dist'); draft.add_argument('tag')
    commands.add_parser('verify-pin')
    args = parser.parse_args()
    if args.operation == 'package': package(args.dist)
    elif args.operation == 'smoke': smoke(args.binary)
    elif args.operation == 'draft-engine': draft_engine(args.dist, args.tag)
    else:
        contents = verified_pin(pin()['tag'])
        verified = dict(line.split('=',1) for line in contents.splitlines() if line and not line.startswith('#'))
        if verified != pin(): raise RuntimeError('Committed pin differs from verified public release.')
