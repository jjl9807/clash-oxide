#!/usr/bin/env python3
"""Keep proxy groups stable across real subscription snapshots and GUI redraws."""
import argparse
import http.server
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


smoke = load('smoke-test')
OUT = smoke.ROOT / 'target/proxy-order'
NAMES = [f'Group {i:02d}' for i in reversed(range(20))] + ['亚洲节点']
YAML = ('proxies: []\nproxy-groups:\n' + ''.join(
    f'  - name: {name}\n    type: select\n    proxies: [DIRECT, REJECT]\n'
    for name in NAMES) + 'rules: ["MATCH,DIRECT"]\n')
EXPECTED = sorted(['GLOBAL', *NAMES])


class Subscription(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = YAML.encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def next_snapshot(daemon, revision):
    def read():
        state = daemon.rpc()
        return state if state['revision'] > revision else None
    return smoke.eventually(read)


def check_order(state):
    groups = state['engine']['groups']
    names = [group['name'] for group in groups]
    assert names == EXPECTED, f'proxy groups moved at revision {state["revision"]}: {names}'
    assert all(group['members'] == ['DIRECT', 'REJECT']
               for group in groups if group['name'] != 'GLOBAL')


def snapshots_test(daemon):
    revision = -1
    for _ in range(5):
        state = next_snapshot(daemon, revision)
        check_order(state)
        revision = state['revision']
    # Mode changes rebuild the engine; its new HashMap must keep the same order.
    for mode in ['global', 'rule']:
        state = daemon.rpc({'SetMode': mode})
        check_order(state)
    print('PASS: remote subscription group order survives periodic snapshots and engine rebuilds', flush=True)


def gui_test(daemon):
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env['CLASH_OXIDE_LANG'] = 'en'
    env['XDG_CONFIG_HOME'] = str(daemon.directory / 'config')
    env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(daemon.directory / 'no-session-bus')
    OUT.mkdir(parents=True, exist_ok=True)
    app = None
    with open(OUT / 'xvfb.log', 'w') as xvfb_log, open(OUT / 'gui.log', 'w') as gui_log:
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', '1', '-screen', '0', '1440x1000x24',
                                 '-nolisten', 'tcp'], env=env, stdout=subprocess.PIPE,
                                stderr=xvfb_log, text=True)
        try:
            number = xvfb.stdout.readline().strip()
            assert number.isdigit()
            env['DISPLAY'] = ':' + number
            app = subprocess.Popen([str(smoke.BINARY), 'gui', '--socket', str(daemon.sock),
                                    '--data-dir', str(daemon.data)], env=env,
                                   stdout=gui_log, stderr=subprocess.STDOUT)

            def xdo(*args):
                return subprocess.check_output(['xdotool', *args], env=env, text=True,
                                               stderr=subprocess.DEVNULL)

            def find_window():
                try:
                    return xdo('search', '--onlyvisible', '--name', '^Clash Oxide$').splitlines()[0]
                except (subprocess.CalledProcessError, IndexError):
                    return None

            window = smoke.eventually(find_window)
            xdo('windowfocus', '--sync', window)
            xdo('mousemove', '--window', window, '1050', '25')
            time.sleep(1)

            def frame():
                # Exclude the live traffic sidebar and the page's status/toolbar.
                return subprocess.check_output(['import', '-display', env['DISPLAY'],
                                                '-window', window, '-crop', '790x500+285+180',
                                                '-depth', '8', 'rgb:-'], env=env)

            def stable(label):
                time.sleep(.3)
                before = frame()
                revision = daemon.rpc()['revision']
                for _ in range(3):
                    state = next_snapshot(daemon, revision)
                    check_order(state)
                    revision = state['revision']
                    time.sleep(.3)  # Allow the native client to consume the snapshot.
                    after = frame()
                    if after != before:
                        (OUT / (label + '-before.rgb')).write_bytes(before)
                        (OUT / (label + '-after.rgb')).write_bytes(after)
                        subprocess.run(['import', '-display', env['DISPLAY'], '-window', window,
                                        str(OUT / (label + '-failure.png'))], env=env, check=True)
                    assert after == before, f'{label}: GUI moved without scroll input'
                subprocess.run(['import', '-display', env['DISPLAY'], '-window', window,
                                str(OUT / (label + '.png'))], env=env, check=True)
                return before

            top = stable('collapsed')
            xdo('mousemove', '--window', window, '700', '450', 'click', '--repeat', '3', '5')
            xdo('mousemove', '--window', window, '1050', '25')
            smoke.eventually(lambda: frame() != top)
            scrolled = stable('scrolled')
            xdo('key', '--clearmodifiers', 'ctrl+f')
            xdo('type', '--clearmodifiers', 'Group')  # Search expands all matching groups.
            xdo('key', '--clearmodifiers', 'Escape')
            smoke.eventually(lambda: frame() != scrolled)
            expanded = stable('expanded')
            xdo('mousemove', '--window', window, '700', '450', 'click', '--repeat', '3', '5')
            xdo('mousemove', '--window', window, '1050', '25')
            smoke.eventually(lambda: frame() != expanded)
            stable('expanded-scrolled')
            print('PASS: native GUI stays still across redraws at the top, after scrolling and with expanded groups', flush=True)
            load('ui-smoke-test').close_window(env['DISPLAY'], window)
            assert app.wait(timeout=10) == 0
            assert daemon.rpc()['phase'] == 'Running'
        finally:
            if app and app.poll() is None:
                app.terminate()
                app.wait(timeout=10)
            xvfb.terminate()
            xvfb.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', action='store_true', help='also check native GUI frames with Xvfb')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='oxide-proxy-order-') as directory:
        daemon = smoke.Daemon(directory)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Subscription)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            daemon.start()
            state = daemon.rpc({'Import': {'name': 'Remote groups', 'source':
                f'http://127.0.0.1:{server.server_address[1]}/subscription.yaml'}})
            assert state['phase'] == 'Running'
            assert state['active_profile'] is not None
            assert state['profiles'][0]['subscription']
            snapshots_test(daemon)
            if args.gui:
                gui_test(daemon)
        finally:
            daemon.stop()
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


if __name__ == '__main__':
    main()
