"""Shared local/CI selection and fail-closed required-result gate."""
import argparse
import json
import os
from pathlib import Path
import subprocess

GROUPS = ('engine', 'ui', 'installer', 'pin', 'release', 'shell', 'rendering', 'tooling')


def classify(paths, full=False):
    selected = set(GROUPS) if full else set()
    for path in paths:
        if path.startswith(('.github/workflows/', 'scripts/tooling/')) or path in ('mise.toml', 'scripts/cargo.sh'):
            selected.update(GROUPS)
        elif path == 'engine/release.pin':
            selected.update(('pin', 'ui', 'installer'))
        elif path.endswith('.md') or path.startswith(('docs/', 'site/', 'branding/', '.github/ISSUE_TEMPLATE/')) or path in ('LICENSE', '.gitignore', '.all-contributorsrc', '.github/FUNDING.yml', '.github/release.yml'):
            pass
        elif path in ('Cargo.toml', 'Cargo.lock', 'engine/Cargo.toml', 'engine/build.rs', 'scripts/build-engine-release.sh') or path.startswith('.cargo/'):
            selected.update(('engine', 'ui', 'release'))
        elif path.startswith(('engine/', 'data/', 'golden/')):
            selected.update(('engine', 'ui'))
            if path == 'engine/tests/rendering.rs': selected.add('rendering')
        elif path in ('run.sh', 'manifest.json', 'scripts/fetch-engine.sh', 'scripts/engine-pin.sh', 'scripts/write-desktop-entry.sh'):
            selected.add('installer')
            if path in ('run.sh', 'manifest.json'): selected.add('ui')
        elif path.startswith(('ui/', 'tests/')):
            selected.add('ui')
            if path.endswith('_test.py'): selected.update(GROUPS)
            if path.startswith('ui/shaders/') or path in ('ui/RadarMap.qml', 'ui/TextureCheck.qml', 'ui/RadarWindow.qml', 'ui/PluginSession.qml', 'tests/map-tiles.qml'):
                selected.add('rendering')
            if path.startswith('tests/integration/') and Path(path).stem in ('bind', 'launcher', 'pin'):
                selected.add('installer')
        elif path in ('scripts/extract-fixtures.sh', 'scripts/refresh-fixtures.sh', 'scripts/bench-engine.py'):
            selected.update(('engine', 'ui'))
            if path == 'scripts/extract-fixtures.sh': selected.add('release')
        elif path == 'scripts/build-shader.sh':
            selected.update(('ui', 'rendering'))
        elif path.startswith('scripts/capture-'):
            pass
        elif path == 'scripts/hooks/omastorm':
            selected.update(('ui', 'installer'))
        else:
            selected.update(GROUPS)
        if path.endswith('.sh') or path == 'scripts/hooks/omastorm': selected.add('shell')
    return {group: group in selected for group in GROUPS}


def git(*args):
    return subprocess.check_output(['git', *args], stderr=subprocess.PIPE)


def changed_paths(base, head='HEAD', local=False):
    if not base or set(base) == {'0'}:
        try:
            base = git('merge-base', 'origin/main', head).decode().strip()
            if base == git('rev-parse', head).decode().strip(): base = None
        except subprocess.CalledProcessError:
            base = None
    else:
        try:
            base = git('merge-base', base, head).decode().strip()
        except subprocess.CalledProcessError:
            base = None
    paths = git('diff', '--name-only', '--no-renames', '-z', base, head) if base else git('ls-files', '-z')
    if local:
        paths += git('diff', '--name-only', '--no-renames', '-z', 'HEAD')
        paths += git('ls-files', '--others', '--exclude-standard', '-z')
    return base, [p for p in paths.decode().split('\0') if p]


def required_jobs(scope):
    return {'changes':True, 'lint':any(scope[k] for k in ('engine','ui','shell','tooling')),
            'engine':scope['engine'], 'plugin':any(scope[k] for k in ('ui','installer','pin')),
            'native-x86':scope['release'], 'native-arm':scope['engine'] or scope['release'], 'bundle':scope['release']}


def gate(jobs):
    outputs = jobs.get('changes', {}).get('outputs', {})
    if set(GROUPS) - outputs.keys() or any(outputs[k] not in ('true','false') for k in GROUPS):
        raise RuntimeError('CI scope is missing or malformed; selected jobs cannot be determined.')
    selected = required_jobs({k:outputs[k]=='true' for k in GROUPS})
    failed = [name for name, enabled in selected.items() if name not in jobs or
              (enabled and jobs[name]['result'] != 'success') or
              (not enabled and jobs[name]['result'] not in ('success','skipped'))]
    if failed: raise RuntimeError('CI did not pass: ' + ', '.join(failed))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='')
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--full', action='store_true')
    parser.add_argument('--gate', action='store_true')
    args = parser.parse_args()
    if args.gate:
        gate(json.loads(os.environ['RESULTS']))
        print('All applicable checks passed.'); return
    base, paths = changed_paths(args.base, args.head)
    groups = classify(paths, args.full or base is None)
    subprocess.run(['git','diff','--check',base,args.head] if base else ['git','show','--format=','--check',args.head], check=True)
    for name in paths:
        path = Path(name)
        if (path.suffix == '.json' or name == '.all-contributorsrc') and path.is_file(): json.loads(path.read_text())
    Path('target').mkdir(exist_ok=True)
    Path('target/ci-changes.json').write_text(json.dumps({'base':base,'paths':paths,'groups':groups},indent=2)+'\n')
    for group, enabled in groups.items(): print(f'{group}={str(enabled).lower()}')
    if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(summary,'a') as out:
            out.write('## Selected checks\n\n| Group | Run |\n|---|---|\n')
            for group, enabled in groups.items():out.write(f'| {group} | {"yes" if enabled else "no"} |\n')
            if groups['rendering']:out.write('\nDesktop GPU checks and review captures are required locally.\n')


if __name__ == '__main__': main()
