#!/usr/bin/env python3
"""Omastorm's nine public commands; argument definitions also generate help."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

from runtime import ROOT, run, Interrupted, handle_interruptions

BENCH = {'recorded': 'scripts/bench-engine.py', 'eccc': 'scripts/bench-eccc.py', 'eccc-live': 'scripts/smoke-eccc.py'}


def parser():
    class Parser(argparse.ArgumentParser):
        def __init__(self, *args, **kwargs):
            kwargs.setdefault('formatter_class', argparse.ArgumentDefaultsHelpFormatter)
            kwargs.setdefault('epilog', 'Exit status: 0 success/help; 1 operation failure; 2 invalid arguments; handled signals 128+signal (dev cleans up and exits 0).')
            super().__init__(*args, **kwargs)
    top = Parser(description=__doc__)
    commands = top.add_subparsers(dest='command', required=True)
    setup = commands.add_parser('setup', help='Fetch locked dependencies and verify fixtures/published engine')
    setup.add_argument('--profile', choices=('desktop', 'engine', 'ui'), default='desktop', help='Prerequisite boundary')
    setup.add_argument('--install-tools', action='store_true', help='Install mise-managed tooling; no privileged packages')
    dev = commands.add_parser('dev', help='Temporary isolated bar plugin; removed on exit')
    dev.add_argument('--engine', choices=('pin', 'candidate'), default='pin', help='Development engine selection')
    test = commands.add_parser('test', help='Fast unit tests, or selected process/UI integration')
    test.add_argument('mode', nargs='?', choices=('unit', 'integration'), default='unit', help='Verification mode')
    test.add_argument('--scope', choices=('all', 'engine', 'protocol', 'ui', 'installer', 'tooling'), default='all', help='Applicable test boundary')
    test.add_argument('--engine', choices=('pin', 'candidate'), default='pin', help='Integration engine selection')
    test.add_argument('--binary', help='Explicit engine path; pin mode verifies the committed checksum')
    test.add_argument('--case', action='append', default=[], help='Repeat UI/installer scenario names for focused integration')
    for name in ('lint', 'format'):
        command = commands.add_parser(name, help='Rust formatting/Clippy, QML/JS syntax and shell checks' if name == 'lint'
                                      else 'Apply rustfmt and regenerate mise task usage; QML/JS has no committed formatter baseline')
        command.add_argument('--scope', choices=('all', 'engine', 'ui', 'tooling'), default='all', help='Language boundary')
    commands.add_parser('build', help='Incremental offline engine and baked shader build')
    check = commands.add_parser('check', help='Complete applicable checks; focused scopes or changed paths')
    check.add_argument('--scope', choices=('all', 'engine', 'protocol', 'ui', 'installer', 'tooling', 'rendering'), default='all', help='Verification boundary')
    check.add_argument('--changed', action='store_true', help='Select complete branch diff plus staged, unstaged and untracked paths')
    check.add_argument('--base', default='origin/main', help='Base for changed-path mode; defaults to origin/main')
    check.add_argument('--gpu', action='store_true', help='Add desktop OpenGL sampling/camera checks')
    bench = commands.add_parser('bench', help='Isolated engine benchmarks; eccc-live alone contacts GeoMet')
    bench.add_argument('harness', nargs='?', choices=tuple(BENCH), default='recorded', help='Recorded NEXRAD baseline, ECCC replay or live ECCC smoke')
    bench.add_argument('options', nargs=argparse.REMAINDER, help='Harness arguments; mise bench -- <harness> --help lists them')
    release = commands.add_parser('release', help='Show release stages without mutation when no stage is supplied')
    stages = release.add_subparsers(dest='product')
    for name in ('engine', 'plugin'):
        product = stages.add_parser(name, help=f'{name.title()} release stages')
        actions = product.add_subparsers(dest='stage', required=True)
        prepare = actions.add_parser('prepare', help='Write version locally on a working branch; no commit/push')
        prepare.add_argument('version')
        actions.add_parser('tag', help='Validate clean current main; push matching tag and draft release')
        if name == 'engine':
            pin = actions.add_parser('pin', help='Verify public release assets before atomic local pin write')
            pin.add_argument('tag')
    return top


def setup(args):
    if args.install_tools:
        run('mise', 'install', timeout=600)
    required = {'engine': (), 'ui': ('quickshell', 'socat'),
                'desktop': ('quickshell', 'socat', 'omarchy', 'omarchy-shell')}[args.profile]
    import shutil
    missing = [name for name in required if not shutil.which(name)]
    if missing:
        raise RuntimeError('Missing desktop packages: ' + ', '.join(missing) + '. Install explicitly with omarchy pkg add; setup never elevates privileges.')
    run('bash', ROOT / 'scripts/extract-fixtures.sh')
    if args.profile in ('desktop', 'engine'):
        run('bash', ROOT / 'scripts/cargo.sh', 'fetch', '--locked', timeout=600)
    if args.profile in ('desktop', 'ui'):
        env = dict(os.environ, XDG_DATA_HOME=str(ROOT / 'target/pinned-data'))
        run('bash', ROOT / 'scripts/fetch-engine.sh', env=env)
    print('Setup complete. Optional capture packages: imagemagick, ffmpeg. Shader/lint/format tools: qt6-shadertools, qt6-declarative. No development/test command fetches dependencies.')


def main():
    os.chdir(ROOT)
    top = parser()
    argv = sys.argv[1:]
    # A bare mise task run carries its parsed usage values in the environment.
    task = os.environ.pop('MISE_TASK_NAME', None)
    if task and argv == [task]:
        import usage
        argv = usage.argv(top, task, os.environ)
    for key in [key for key in os.environ if key.startswith('usage_')]:
        del os.environ[key]
    args = top.parse_args(argv)
    if args.command == 'release' and not args.product:
        top.parse_args(['release', '--help'])
    if args.command != 'dev':
        handle_interruptions()
    try:
        if args.command == 'setup':
            setup(args)
        elif args.command == 'dev':
            from dev import develop
            develop(args)
        elif args.command == 'build':
            run('bash', ROOT / 'scripts/cargo.sh', 'build', '--offline', '--locked', '--target-dir', ROOT / 'target', timeout=600)
            run('bash', ROOT / 'scripts/build-shader.sh')
        elif args.command == 'test':
            from suite import unit, integration, validate_integration
            if args.mode == 'unit':
                if args.scope not in ('all', 'engine', 'ui', 'tooling') or args.case or args.binary or args.engine != 'pin':
                    top.error('Unit mode supports all/engine/ui/tooling; engine selection and cases require integration.')
                unit(args.scope)
            else:
                try: validate_integration(args)
                except ValueError as error: top.error(str(error))
                integration(args)
        elif args.command in ('lint', 'format'):
            # qmlformat parses without rewriting: a syntax check, not a style
            # gate. Most UI files predate qmlformat, so format leaves them alone.
            if args.command == 'lint' and args.scope in ('all', 'ui'):
                for file in sorted(list((ROOT / 'ui').glob('*.qml')) + list((ROOT / 'ui').glob('*.js'))):
                    run('/usr/lib/qt6/bin/qmlformat', file, stdout=subprocess.DEVNULL)
                run('/usr/lib/qt6/bin/qmllint', *sorted((ROOT / 'ui').glob('*.js')))
            if args.command == 'format' and args.scope in ('all', 'tooling'):
                import usage
                usage.write(top)
            if args.scope in ('all', 'engine'):
                run('bash', ROOT / 'scripts/cargo.sh', 'fmt', *(['--check'] if args.command == 'lint' else []))
                if args.command == 'lint':
                    run('bash', ROOT / 'scripts/cargo.sh', 'clippy', '--offline', '--locked', '--all-targets', '--', '-D', 'warnings', timeout=600)
            if args.command == 'lint' and args.scope in ('all', 'tooling'):
                run('shellcheck', '-x', ROOT / 'run.sh', ROOT / 'scripts/hooks/omastorm', *sorted((ROOT / 'scripts').glob('*.sh')), *sorted((ROOT / 'tests/integration').glob('*.sh')))
        elif args.command == 'check':
            from suite import check
            check(args)
        elif args.command == 'bench':
            # Replays wait through real retirement grace; no wall-clock cap.
            run('python3', ROOT / BENCH[args.harness], *args.options, timeout=None)
        else:
            from release import execute
            execute(args)
    except Interrupted as error:
        return 128 + error.signum
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
