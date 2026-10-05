#!/usr/bin/env python3
"""Check real CLI/IPC and live TUI language changes with private preferences."""
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


smoke = load_script('smoke-test')
lifecycle = load_script('lifecycle-test')


def main():
    en = json.loads((smoke.ROOT / 'crates/i18n/locales/en.json').read_text())
    for source in (smoke.ROOT / 'crates').glob('*/src/**/*.rs'):
        text = source.read_text().split('#[cfg(test)]')[0]
        for key in re.findall(r'"((?:nav|language|theme|common|phase|mode|status|app|overview|proxies|profiles|connections|settings|tray|tui|cli|error|sidebar|rules|ui|logs)\.[a-z_]+)"', text):
            assert key in en, f'Missing translation: {source}: {key}'

    with tempfile.TemporaryDirectory(prefix='oxide-i18n-') as directory:
        base = Path(directory)
        os.environ['XDG_CONFIG_HOME'] = str(base / 'config')
        os.environ['LC_ALL'] = 'C.UTF-8'
        os.environ.pop('CLASH_OXIDE_LANG', None)
        os.environ.pop('LANGUAGE', None)
        preferences = base / 'config/clash-oxide/frontend.json'
        daemon = smoke.Daemon(base)

        def cli(*args, overrides=None):
            env = os.environ.copy()
            env.update(overrides or {})
            return subprocess.run([str(smoke.BINARY), *args, '--socket', str(daemon.sock),
                '--data-dir', str(daemon.data)], env=env, capture_output=True, text=True, timeout=20)

        assert 'Usage:' in cli('--help').stdout
        assert '用法:' in cli('--lang', 'zh-CN', '--help').stdout
        assert '用法:' in cli('--help', overrides={'LC_ALL': 'zh_CN.UTF-8'}).stdout
        assert 'Usage:' in cli('--help', overrides={'LANGUAGE': 'zh_CN'}).stdout  # C locale
        assert '用法:' in cli('--help', overrides={'CLASH_OXIDE_LANG': 'zh-CN'}).stdout
        assert 'Usage:' in cli('--lang=en', '--help', overrides={'CLASH_OXIDE_LANG': 'zh-CN'}).stdout
        assert not preferences.exists(), 'Read-only commands must not create preferences'
        preferences.parent.mkdir(parents=True)
        preferences.write_text('{bad json')
        assert cli('--help').returncode == 0
        assert preferences.read_text() == '{bad json'
        preferences.write_text('{"language":"zh-CN"}')
        assert '用法:' in cli('--help').stdout
        assert 'Usage:' in cli('--lang=auto', '--help').stdout
        assert 'Usage:' in cli('--help', overrides={'CLASH_OXIDE_LANG': 'en'}).stdout
        preferences.unlink()
        print('PASS: embedded CLI help, system/env/flag/preference priority, corrupt-preference fallback', flush=True)

        tui = None
        try:
            daemon.start()
            pid = lifecycle.peer_pid(daemon.sock)
            with socket.socket(socket.AF_UNIX) as connection:
                connection.connect(str(daemon.sock))
                connection.sendall(json.dumps({'version': 2, 'id': 91, 'request': {'Command': {'SetMixedPort': 0}}}).encode() + b'\n')
                response = json.loads(connection.makefile('rb').readline())
            assert response['error']  # old clients still have their original string
            assert response['diagnostic']['code'] == 'error.port', response
            state = json.loads(cli('--lang=zh-CN', 'ctl', 'status').stdout)
            assert state['phase'] == 'Running'
            assert state['last_diagnostic']['code'] == 'error.port'
            result = cli('--lang=zh-CN', 'ctl', 'port', '0')
            zh = json.loads((smoke.ROOT / 'crates/i18n/locales/zh-CN.json').read_text())
            assert result.returncode != 0 and zh['error.port'] in result.stderr
            source = base / 'source.yaml'
            source.write_text(smoke.YAML)
            daemon.configure(source)
            tui = lifecycle.Tui(daemon)
            smoke.eventually(lambda: b'Daemon connected' in tui.output)
            os.write(tui.master, b'\x1b[17~')  # F6 Settings
            smoke.eventually(lambda: b'Latency' in tui.output)
            os.write(tui.master, b'\x1b[B' * 4 + b'\r')  # Simplified Chinese
            smoke.eventually(lambda: preferences.exists() and json.loads(preferences.read_text())['language'] == 'zh-CN')
            smoke.eventually(lambda: zh['language.title'] in re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', tui.output.decode(errors='replace')))
            assert lifecycle.peer_pid(daemon.sock) == pid
            assert daemon.rpc()['phase'] == 'Running'
            assert '用法:' in cli('--help').stdout
            # User-entered Unicode remains unchanged under the Chinese interface.
            os.write(tui.master, b'\x1bOQn')  # F2 Profiles, import dialog
            time.sleep(.3)
            os.write(tui.master, '中文配置'.encode() + b'\t')
            os.write(tui.master, b'\x1b[200~' + str(source).encode() + b'\x1b[201~\r')
            smoke.eventually(lambda: any(p['name'] == '中文配置' for p in daemon.rpc()['profiles']))
            tui.close()
            tui = lifecycle.Tui(daemon)
            smoke.eventually(lambda: zh['status.connected'] in smoke.terminal_title(tui.output))
            assert lifecycle.peer_pid(daemon.sock) == pid
            print('PASS: structured IPC errors, stable JSON values, live TUI switch/persistence, Unicode import, daemon unchanged', flush=True)
        finally:
            if tui:
                (smoke.ROOT / 'target/ui-smoke/i18n-tui.ansi').write_bytes(tui.output)
                tui.close()
            daemon.shutdown()
            daemon.stop()


if __name__ == '__main__':
    main()
