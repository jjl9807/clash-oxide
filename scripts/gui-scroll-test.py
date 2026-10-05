#!/usr/bin/env python3
"""Stress native proxy/rule lists and exercise their scrollbar, search and resize."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


smoke = load('smoke-test')
OUT = smoke.ROOT / 'target/gui-scroll'
GROUPS = [f'Group {i:02d}' for i in range(24)]
NODES = [f'Node {i:03d}' for i in range(160)]
YAML = ('proxies:\n' + ''.join(
    f'  - name: {node}\n    type: socks5\n    server: 127.0.0.1\n    port: 9\n'
    for node in NODES) + 'proxy-groups:\n' + ''.join(
    f'  - name: {group}\n    type: select\n    proxies: ' + json.dumps(NODES) + '\n'
    for group in GROUPS) + 'rules:\n' + ''.join(
    f'  - DOMAIN,rule{i:04d}.example,DIRECT\n' for i in range(4999)) + '  - MATCH,DIRECT\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--benchmark-only', action='store_true', help='measure wheels on older builds too')
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='oxide-gui-scroll-') as directory:
        daemon = smoke.Daemon(directory)
        base = Path(directory)
        source = base / 'large.yaml'
        source.write_text(YAML)
        preferences = base / 'config/clash-oxide/view.json'
        preferences.parent.mkdir(parents=True)
        preferences.write_text(json.dumps({'expanded': GROUPS}))
        env = os.environ.copy()
        env.pop('WAYLAND_DISPLAY', None)
        env['XDG_CONFIG_HOME'] = str(base / 'config')
        env['CLASH_OXIDE_LANG'] = 'en'
        env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(base / 'no-session-bus')
        app = xvfb = None
        try:
            daemon.start()
            daemon.configure(source)
            assert len(daemon.rpc()['engine']['rules']) == 5000
            with open(OUT / 'xvfb.log', 'w') as log:
                xvfb = subprocess.Popen(['Xvfb', '-displayfd', '1', '-screen', '0',
                                         '1440x1000x24', '-nolisten', 'tcp'], env=env,
                                        stdout=subprocess.PIPE, stderr=log, text=True)
            number = xvfb.stdout.readline().strip()
            assert number.isdigit()
            env['DISPLAY'] = ':' + number
            with open(OUT / 'gui.log', 'w') as log:
                app = subprocess.Popen([str(smoke.BINARY), 'gui', '--socket', str(daemon.sock),
                                        '--data-dir', str(daemon.data)], env=env,
                                       stdout=log, stderr=subprocess.STDOUT)

            def xdo(*command):
                return subprocess.check_output(['xdotool', *command], env=env, text=True,
                                               stderr=subprocess.DEVNULL)

            def find_window():
                try:
                    return xdo('search', '--onlyvisible', '--name', '^Clash Oxide$').splitlines()[0]
                except (subprocess.CalledProcessError, IndexError):
                    assert app.poll() is None, (OUT / 'gui.log').read_text()
                    return None

            window = smoke.eventually(find_window, timeout=30)
            xdo('windowfocus', '--sync', window)
            xdo('windowmove', window, '0', '0')

            def frame():
                return subprocess.check_output(['import', '-display', env['DISPLAY'],
                                                '-window', window, '-crop', '750x440+285+210',
                                                '-depth', '8', 'rgb:-'], env=env)

            def shot(name):
                subprocess.run(['import', '-display', env['DISPLAY'], '-window', window,
                                str(OUT / (name + '.png'))], env=env, check=True)

            def settle():
                previous = frame()
                for _ in range(30):
                    time.sleep(.2)
                    current = frame()
                    if current == previous:
                        return current
                    previous = current
                raise AssertionError('viewport did not settle')

            def cpu_time():
                fields = Path(f'/proc/{app.pid}/stat').read_text().split(') ', 1)[1].split()
                return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')

            def wheel(label):
                before = settle()
                xdo('mousemove', '--window', window, '700', '450')
                started = time.monotonic()
                cpu = cpu_time()
                xdo('click', '--repeat', '40', '--delay', '15', '5')
                smoke.eventually(lambda: frame() != before, timeout=30)
                after = settle()
                print(f'{label}: 40 wheel events, {time.monotonic() - started:.2f}s wall, '
                      f'{cpu_time() - cpu:.2f}s GUI CPU', flush=True)
                assert after != before
                shot(label + '-wheel')
                return after

            time.sleep(1.2)
            shot('proxies-top')
            wheel('proxies')
            if not args.benchmark_only:
                # Scrollbar gutter at the right edge of the page viewport.
                before = settle()
                xdo('mousemove', '--window', window, '1076', '185', 'click', '1')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('proxies-track')
                # Jump to the final group via the track, then drag its thumb.
                before = frame()
                xdo('mousemove', '--window', window, '1076', '700', 'click', '1')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('proxies-bottom')
                before = frame()
                xdo('mousemove', '--window', window, '1076', '690', 'mousedown', '1',
                    'mousemove', '--window', window, '1076', '350', 'mouseup', '1')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('proxies-drag')
                before = frame()
                xdo('key', '--clearmodifiers', 'ctrl+f')
                xdo('type', '--clearmodifiers', 'Node 159')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('proxies-search')
                xdo('mousemove', '--window', window, '450', '198', 'click', '1')
                smoke.eventually(lambda: next(g for g in daemon.rpc()['engine']['groups']
                                             if g['name'] == GROUPS[0])['selected'] == NODES[-1])
                xdo('key', '--clearmodifiers', 'ctrl+f', 'ctrl+a', 'BackSpace')
                time.sleep(.3)
                xdo('key', '--clearmodifiers', 'Escape')
                settle()

            previous = frame()
            xdo('key', '--clearmodifiers', 'ctrl+4')
            smoke.eventually(lambda: frame() != previous, timeout=30)
            settle()
            shot('rules-top')
            wheel('rules')
            if not args.benchmark_only:
                before = frame()
                xdo('mousemove', '--window', window, '1076', '700', 'click', '1')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('rules-bottom')
                before = frame()
                xdo('mousemove', '--window', window, '1076', '690', 'mousedown', '1',
                    'mousemove', '--window', window, '1076', '350', 'mouseup', '1')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('rules-drag')
                before = frame()
                xdo('key', '--clearmodifiers', 'ctrl+f')
                xdo('type', '--clearmodifiers', 'rule4998.example')
                smoke.eventually(lambda: frame() != before)
                settle()
                shot('rules-search')
                # Resizing must recompute grid columns and preserve usable scrollbars.
                xdo('key', '--clearmodifiers', 'ctrl+1')
                xdo('windowsize', window, '760', '540')
                time.sleep(.5)
                shot('proxies-narrow')
                xdo('windowsize', window, '1300', '900')
                time.sleep(.5)
                shot('proxies-wide')
                print('PASS: large proxy/rule lists, wheel/track/thumb input, search, node selection and resize', flush=True)
            load('ui-smoke-test').close_window(env['DISPLAY'], window)
            assert app.wait(timeout=10) == 0
        finally:
            if app and app.poll() is None:
                app.terminate()
                app.wait(timeout=10)
            if xvfb:
                xvfb.terminate()
                xvfb.wait(timeout=5)
            daemon.stop()


if __name__ == '__main__':
    main()
