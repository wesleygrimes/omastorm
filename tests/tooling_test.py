"""Tooling regressions run with mocked host commands and isolated installs."""
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts/tooling'))
import cli
import dev


class Commands(unittest.TestCase):
    def test_tooling_lint_covers_the_extensionless_theme_hook(self):
        with patch('sys.argv', ['omastorm', 'lint', '--scope', 'tooling']), patch.object(cli, 'run') as run:
            self.assertEqual(cli.main(), 0)
        argv = run.call_args.args
        self.assertEqual(argv[0], 'shellcheck')
        self.assertIn(cli.ROOT / 'scripts/hooks/omastorm', argv)

    def test_invalid_integration_usage_exits_two_before_starting_processes(self):
        for argv in (['test','integration','--case','nonexistent-case'],['test','integration','--scope','protocol','--case','popover']):
            with patch('sys.argv',['omastorm',*argv]),patch('suite.integration') as start,contextlib.redirect_stderr(io.StringIO()),self.assertRaises(SystemExit) as error:
                cli.main()
            self.assertEqual(error.exception.code,2)
            start.assert_not_called()

    def test_nine_public_commands_and_help(self):
        p = cli.parser()
        self.assertEqual(set(p._subparsers._group_actions[0].choices),
                         {'setup', 'dev', 'test', 'lint', 'format', 'build', 'check', 'bench', 'release'})
        self.assertEqual(p.parse_args(['bench']).harness, 'recorded')
        self.assertEqual(p.parse_args(['bench', 'eccc', '--runs', '1']).options, ['--runs', '1'])
        for harness in cli.BENCH.values():
            self.assertTrue((cli.ROOT / harness).is_file(), harness)
        self.assertIsNone(p.parse_args(['release']).product)
        self.assertEqual(p.parse_args(['dev']).engine, 'pin')
        with self.assertRaises(SystemExit) as error:
            p.parse_args(['dev', '--unknown'])
        self.assertEqual(error.exception.code, 2)


class Lifecycle(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.host = patch.dict(os.environ, HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / 'ignored-xdg'))
        self.host.start()
        self.calls = []
        def command(*args, **kwargs):
            self.calls.append(tuple(map(str, args)))
            class Result:
                stdout = json.dumps([{'id': dev.ID}])
            if tuple(args) == ('omarchy-shell', 'omastorm-dev', 'status'):
                root = self.root / '.config/omarchy/plugins' / dev.ID
                directory = (root / json.loads((root / 'manifest.json').read_text())['entryPoints']['barWidget']).parent
                settings = json.loads((directory / 'Instance.js').read_text().split('var settings = ', 1)[1].rstrip(';\n'))
                Result.stdout = json.dumps(dict(revision=settings['revision'], pluginId=dev.ID, runtime=settings['runtime'], connected=True))
            return Result()
        self.mock = patch.object(dev, 'run', side_effect=command)
        self.mock.start()

    def tearDown(self):
        self.mock.stop()
        self.host.stop()
        self.tmp.cleanup()

    def staged(self):
        s = dev.Session('/unused')
        s.plugins.mkdir(parents=True, exist_ok=True)
        s.lock = (s.plugins / f'.{dev.ID}.lock').open('a')
        import fcntl
        fcntl.flock(s.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        s.dest.mkdir()
        s.runtime = self.root / 'runtime'
        s.runtime.mkdir()
        s.cache = self.root / 'cache'
        s.cache.mkdir()
        s.write_owner()
        return s

    def test_staging_routes_own_runtime_and_preserves_production(self):
        s = self.staged()
        production = s.plugins / 'com.omastorm.radar'
        production.mkdir()
        (production / 'sentinel').write_text('preserved')
        try:
            s.reload()
            first = json.loads((s.dest / 'manifest.json').read_text())
            s.reload()
            second = json.loads((s.dest / 'manifest.json').read_text())
            self.assertEqual(second['id'], dev.ID)
            self.assertNotEqual(first['entryPoints'], second['entryPoints'])
            directory = (s.dest / second['entryPoints']['barWidget']).parent
            settings = (directory / 'Instance.js').read_text()
            self.assertIn(str(s.runtime), settings)
            self.assertIn(dev.ID, settings)
            self.assertTrue((directory / 'qmldir').exists())
            self.assertTrue((directory / 'shaders/radar.frag.qsb').exists())
            self.assertTrue((s.dest / 'engine/release.pin').exists())
            self.assertFalse(any(path.is_symlink() for path in s.dest.rglob('*')), 'Host validator rejects all staged symlinks')
            self.assertEqual(sum(call == ('omarchy-shell', 'shell', 'rescanPlugins') for call in self.calls), 2)
            with self.assertRaisesRegex(RuntimeError, 'Another worktree'):
                other = dev.Session('/unused')
                try:
                    other.acquire()
                finally:
                    other.close()
            self.assertTrue(s.dest.exists())
        finally:
            s.close()
        self.assertEqual((production / 'sentinel').read_text(), 'preserved')
        self.assertFalse(s.dest.exists())

    def test_foreign_install_refused_without_commands(self):
        s = dev.Session('/unused')
        s.dest.mkdir(parents=True)
        (s.dest / 'sentinel').write_text('foreign')
        try:
            with self.assertRaisesRegex(RuntimeError, 'unowned'):
                s.acquire()
        finally:
            s.close()
        self.assertEqual((s.dest / 'sentinel').read_text(), 'foreign')
        self.assertFalse(self.calls)

    def test_cleanup_failure_is_reported_and_artifacts_retained(self):
        s = self.staged()
        s.reload()
        with patch.object(dev, 'run', side_effect=RuntimeError('shell unavailable')):
            with self.assertRaisesRegex(RuntimeError, 'cleanup incomplete'):
                s.close()
        self.assertTrue(s.dest.exists())


if __name__ == '__main__':
    unittest.main()
