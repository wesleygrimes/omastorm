"""Focused verification using one engine selector and owned scratch/processes."""
import contextlib
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

from runtime import ROOT, Process, binary, run, wait_until

UI = ('engine-ui', 'radar-handoff', 'map-sites', 'map-network', 'location', 'picker', 'keys',
      'ip-location', 'theme', 'map-tiles', 'popover', 'reconnect')
INSTALLER = ('bind', 'launcher', 'pin')
NO_DAEMON = {'bind', 'launcher', 'pin', 'theme', 'map-tiles', 'ip-location'}


@contextlib.contextmanager
def providers(evidence=None):
    """Recorded vector data, deterministic TileJSON; no missing-tile fallback."""
    requests = []
    class Handler(BaseHTTPRequestHandler):
        def do_CONNECT(self):
            requests.append({'method':'CONNECT', 'path':self.path})
            try:
                self.send_error(502, 'Live providers disabled by integration fixture proxy')
            except (BrokenPipeError, ConnectionResetError):
                pass
        def do_GET(self):
            requests.append({'method':'GET', 'path':self.path})
            if self.path == '/tiles.json':
                payload = json.dumps({'name': 'Recorded test tiles', 'attribution': '© OpenStreetMap contributors',
                                      'version': 'test-fixture', 'tiles': [f'http://127.0.0.1:{self.server.server_port}/{{z}}/{{x}}/{{y}}.pbf']}).encode()
            elif self.path.endswith('.pbf'):
                payload = (ROOT / 'engine/tests/fixtures/vt/7-29-50.pbf').read_bytes()
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        def log_message(self, *args):
            pass
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}/tiles.json'
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        if evidence:
            (evidence / f'providers-{time.time_ns()}.json').write_text(json.dumps(requests, indent=2) + '\n')


class Report:
    def __init__(self, name):
        self.root = ROOT / 'target/evidence' / (name + '-' + str(time.time_ns()))
        self.root.mkdir(parents=True)
        self.start = time.monotonic()
        self.rows = []

    def step(self, name, argv, env=None, timeout=600):
        start = time.monotonic()
        log = self.root / (name + '.log')
        process = Process(argv, env or os.environ.copy(), log)
        try:
            status = process.child.wait(timeout=timeout)
        finally:
            process.close()
        elapsed = time.monotonic() - start
        self.rows.append(dict(step=name, command=list(map(str,argv)), status=status, seconds=elapsed))
        print(f'{name}: {"PASS" if status == 0 else "FAIL"} {elapsed:.2f}s ({log})', flush=True)
        if status:
            print(log.read_text()[-12000:])
            raise RuntimeError(f'{name} failed; see {log}')

    def finish(self):
        output = dict(commit=run('git', 'rev-parse', 'HEAD', capture_output=True, text=True).stdout.strip(),
                      tree=run('git', 'status', '--porcelain', capture_output=True, text=True).stdout,
                      steps=self.rows, wall=time.monotonic() - self.start)
        (self.root / 'timings.json').write_text(json.dumps(output, indent=2) + '\n')
        print(f'Verification evidence: {self.root} (wall {output["wall"]:.2f}s)', flush=True)


def cargo(*args):
    return ['bash', str(ROOT / 'scripts/cargo.sh'), *args, '--offline', '--locked']


def unit(scope='all'):
    report = Report('unit')
    try:
        if scope in ('all', 'engine'):
            report.step('engine-unit', cargo('test', '--bin', 'omastorm-engine'))
            report.step('cpu-rendering', cargo('test', '--test', 'rendering'))
        if scope in ('all', 'ui'):
            report.step('ui-logic', ['node', 'tests/ui-unit.cjs'])
        if scope in ('all', 'tooling'):
            report.step('tooling', ['python3', '-m', 'unittest', 'discover', '-s', 'tests', '-p', '*_test.py'])
    finally:
        report.finish()


def validate_integration(args):
    requested = args.case or (list(UI + INSTALLER) if args.scope in ('all', 'ui') else
                             list(INSTALLER) if args.scope == 'installer' else [])
    unknown = set(requested) - set(UI + INSTALLER)
    if unknown:
        raise ValueError('Unknown integration scenarios: ' + ', '.join(sorted(unknown)))
    if args.case and args.scope not in ('all', 'ui', 'installer'):
        raise ValueError('--case selects UI/installer scenarios; use --scope ui, installer, or all')
    return requested


def integration(args):
    requested = validate_integration(args)
    report = Report('integration')
    # Scenarios retaining their own fixed log paths cannot overlap in one tree.
    (ROOT / 'target').mkdir(exist_ok=True)
    with (ROOT / 'target/integration.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another integration runner owns this worktree.') from None
        try:
            if args.scope in ('all', 'engine', 'protocol') and not args.case:
                with providers(report.root) as tiles, tempfile.TemporaryDirectory(prefix='omastorm-protocol-', dir='/tmp') as scratch:
                    env = isolated_environment(Path(scratch), tiles)
                    report.step('protocol', cargo('test', '--test', 'protocol'), env)
                    # Loopback WMS only; uses the debug binary the protocol tests built.
                    report.step('eccc-contract', ['python3', 'scripts/test-eccc.py'], env, timeout=180)

            if args.scope == 'tooling':
                report.step('tooling', ['python3', '-m', 'unittest', 'discover', '-s', 'tests', '-p', '*_test.py'])
            if not requested:
                return
            engine = binary(args.engine, args.binary)
            import hashlib
            report.rows.append(dict(selected_engine=args.engine, binary=str(engine), sha256=hashlib.sha256(engine.read_bytes()).hexdigest()))
            if args.engine == 'candidate':
                # Matching numbers alone never replace published coverage.
                pin_engine = binary('pin')
                report.rows.append(dict(selected_engine='pin', binary=str(pin_engine), sha256=hashlib.sha256(pin_engine.read_bytes()).hexdigest()))
                run_cases(requested, pin_engine, report, 'pin')
                from release import smoke
                smoke(engine)
                import re
                source_protocol = re.findall(r'^pub const VERSION: u32 = (\d+);$', (ROOT / 'engine/src/protocol.rs').read_text(), re.M)
                ui_protocol = re.findall(r'message\.v !== (\d+)', (ROOT / 'ui/Engine.qml').read_text())
                if len(source_protocol) != 1 or len(ui_protocol) != 1:
                    raise RuntimeError('Could not determine unique engine/UI protocol versions.')
                if source_protocol != ui_protocol:
                    print('Candidate/UI protocols differ: published UI coverage passed; test source engine independently.', flush=True)
                    return
            run_cases(requested, engine, report, args.engine, pin_engine if args.engine == 'candidate' else engine)
        finally:
            report.finish()


def isolated_environment(case, tiles):
    env = dict(os.environ, TMPDIR=str(case), XDG_RUNTIME_DIR=str(case / 'r'),
               XDG_CACHE_HOME=str(case / 'cache'), XDG_DATA_HOME=str(case / 'data'),
               OMASTORM_ROOT=str(ROOT), OMASTORM_ARCHIVE=str(ROOT / 'data/raw/KTLX20130520_201643_V06.gz'),
               OMASTORM_TILES_URL=tiles, OMASTORM_METAR_FIXTURE=str(ROOT / 'engine/tests/fixtures/metar-ktlx.json'),
               OMASTORM_STATIONS_FIXTURE=str(ROOT / 'engine/tests/fixtures/stations-ktlx.json'),
               QT_QPA_PLATFORM='offscreen', QT_QPA_PLATFORMTHEME='basic', QT_QUICK_BACKEND='rhi', QSG_RHI_BACKEND='opengl')
    env.update({key: tiles.rsplit('/', 1)[0] for key in ('http_proxy','https_proxy','all_proxy','HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')})
    env.update(no_proxy='127.0.0.1,localhost', NO_PROXY='127.0.0.1,localhost')
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('OMASTORM_RESCAN_PLUGIN', None)
    for key in ('OMASTORM_ENGINE_BINARY','OMASTORM_ENGINE_PIN','OMASTORM_ENGINE_ASSET','OMASTORM_CONFIG','OMASTORM_STATE','OMASTORM_LOCATION','OMASTORM_QML'):
        env.pop(key, None)
    Path(env['XDG_RUNTIME_DIR']).mkdir(exist_ok=True)
    return env


def run_cases(names, engine, report, mode, pinned=None):
    with providers(report.root) as tiles, tempfile.TemporaryDirectory(prefix='omastorm-tests-', dir='/tmp') as scratch:
        for name in names:
            case = Path(scratch) / name
            case.mkdir()
            env = isolated_environment(case, tiles)
            env.update(OMASTORM_ENGINE_BINARY=str(engine), OMASTORM_PINNED_BINARY=str(pinned or engine))
            daemon = None
            try:
                if name not in NO_DAEMON and name not in ('popover', 'reconnect'):
                    daemon = Process([engine, 'serve'], env, report.root / (mode + '-' + name + '-engine.log'))
                    wait_until(lambda: (Path(env['XDG_RUNTIME_DIR']) / 'omastorm/engine.sock').exists(), process=daemon.child)
                report.step(mode + '-' + name, ['bash', f'tests/integration/{name}.sh'], env, timeout=180)
            finally:
                # Some scenarios deliberately stop/restart this scratch daemon.
                # Stop only the selected binary in this case's runtime.
                if daemon:
                    daemon.close()
                with contextlib.suppress(subprocess.CalledProcessError):
                    run(engine, 'stop', env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def check(args):
    scopes = {args.scope}
    if args.changed:
        import selection
        base, paths = selection.changed_paths(args.base, 'HEAD', local=True)
        groups = selection.classify(paths, full=base is None)
        scopes = {'tooling'} | {scope for scope in ('engine','ui','installer') if groups[scope]}
        run('git', 'diff', '--check', base or 'HEAD', timeout=30)
        if groups['engine']:
            scopes.add('protocol')
        if groups['release'] or groups['shell'] or groups['tooling']:
            scopes.add('tooling')
        if groups['shell']:
            run('python3', 'scripts/tooling/cli.py', 'lint', '--scope', 'tooling', timeout=600)
        if groups['release']:
            print('Native platform/artifact validation is required in CI on x86_64 and aarch64.', flush=True)
        if groups['rendering'] and not args.gpu:
            print('Rendering paths changed: run mise check --gpu --scope rendering and attach review captures.', flush=True)
        if groups['pin']:
            run('python3', 'scripts/tooling/release.py', 'verify-pin', timeout=600)
        print('Changed-path scopes: ' + ', '.join(sorted(scopes)), flush=True)
    from argparse import Namespace
    if 'all' in scopes:
        run('python3', 'scripts/tooling/cli.py', 'lint', timeout=600)
        unit()
        integration(Namespace(scope='all', case=[], engine='pin', binary=None))
        run('python3', 'scripts/tooling/release.py', 'verify-pin', timeout=600)
    else:
        for scope in sorted(scopes):
            if scope == 'engine':
                run('python3', 'scripts/tooling/cli.py', 'lint', '--scope', 'engine', timeout=600)
                unit('engine')
            elif scope == 'ui':
                run('python3', 'scripts/tooling/cli.py', 'lint', '--scope', 'ui')
                unit('ui')
                integration(Namespace(scope='ui', case=[], engine='pin', binary=None))
            elif scope in ('protocol','installer'):
                integration(Namespace(scope=scope, case=[], engine='pin', binary=None))
            elif scope == 'tooling':
                unit('tooling')
    run('python3', 'scripts/tooling/docs.py')
    if args.gpu or 'rendering' in scopes:
        report = Report('gpu')
        try:
            report.step('rendering', ['bash', ROOT / 'scripts/cargo.sh', 'test', '--offline', '--locked', '--test', 'rendering', '--', '--ignored'],
                        env=dict(os.environ, QT_QPA_PLATFORM='offscreen'), timeout=600)
        finally:
            report.finish()
