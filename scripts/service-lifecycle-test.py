#!/usr/bin/env python3
"""Test service discovery using a systemctl stub in isolated user/mount/network namespaces.

Run with: unshare -Urnm python3 scripts/service-lifecycle-test.py
The host service manager is never contacted and host /etc and /run are untouched.
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('lifecycle', Path(__file__).with_name('lifecycle-test.py'))
lifecycle = importlib.util.module_from_spec(spec); spec.loader.exec_module(lifecycle)
smoke = lifecycle.smoke


def main():
    mapping = Path('/proc/self/uid_map').read_text().split()
    assert len(mapping) == 3 and mapping[0] == '0' and mapping[2] == '1', 'Use unshare -Urnm'
    # Create our own mount namespace explicitly; /proc/1 namespace links may be unreadable.
    os.unshare(os.CLONE_NEWNS)
    subprocess.run(['mount', '--make-rprivate', '/'], check=True)
    with tempfile.TemporaryDirectory(prefix='oxide-service-') as temporary:
        base = Path(temporary)
        for name in ['etc/clash-oxide', 'run', 'tools', 'runtime']:
            (base / name).mkdir(parents=True)
        for name in ['etc', 'run']:
            subprocess.run(['mount', '--bind', str(base / name), '/' + name], check=True)
        try:
            Path('/etc/clash-oxide/0.env').write_text('# Service installed for this mapped user\n')
            os.environ['XDG_DATA_HOME'] = str(base / 'data')
            os.environ['XDG_RUNTIME_DIR'] = str(base / 'runtime')
            os.environ['OXIDE_TEST_SERVICE_DIR'] = str(base)
            os.environ['OXIDE_TEST_SERVICE_BINARY'] = str(smoke.BINARY)
            os.environ['PATH'] = str(base / 'tools') + ':' + os.environ['PATH']
            shim = base / 'tools/systemctl'
            shim.write_text('''#!/usr/bin/env python3
import os, subprocess, sys
from pathlib import Path
base = Path(os.environ['OXIDE_TEST_SERVICE_DIR'])
assert sys.argv[1:] == ['--no-ask-password', '--no-block', 'start', 'clash-oxide@0.service']
with (base / 'calls').open('a') as f: f.write('start\\n')
if (base / 'fail').exists(): sys.exit(1)
with (base / 'service.log').open('a') as log:
    subprocess.Popen([os.environ['OXIDE_TEST_SERVICE_BINARY'], 'daemon', '--socket',
        '/run/clash-oxide-0/control.sock', '--data-dir', str(base / 'data/clash-oxide')],
        stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
''')
            shim.chmod(0o755)
            daemon = smoke.Daemon(base)
            daemon.data = base / 'data/clash-oxide'
            daemon.sock = Path('/run/clash-oxide-0/control.sock')
            daemon.seed()
            clients = []
            try:
                clients = [lifecycle.Tui(daemon, managed=True), lifecycle.Tui(daemon, managed=True)]
                smoke.eventually(lambda: daemon.rpc()['revision'] > 0)
                for client in clients:
                    smoke.eventually(lambda: 'Daemon connected' in smoke.terminal_title(client.output))
                assert (base / 'calls').read_text() == 'start\n', 'Concurrent UIs must start one service'
                assert not (daemon.data / 'daemon.log').exists(), 'Do not launch an unprivileged fallback'
                result = subprocess.run([str(smoke.BINARY), 'kill'], capture_output=True, text=True, timeout=160)
                assert result.returncode == 0, result.stderr
                assert 'stopped' in result.stdout
                time.sleep(3)
                assert not daemon.sock.exists()
                assert (base / 'calls').read_text() == 'start\n'
                for client in clients: client.close()
                clients.clear()
                # Failed service activation is shown to the user; no ordinary daemon is spawned.
                (base / 'fail').touch()
                clients.append(lifecycle.Tui(daemon, managed=True))
                smoke.eventually(lambda: b'Cannot start the installed service' in clients[0].output)
                assert not (daemon.data / 'daemon.log').exists()
                assert not daemon.sock.exists()
                print('PASS: installed service discovery, concurrent activation, default kill, no respawn, service errors without fallback (systemctl stub, private mounts)')
            finally:
                for client in clients: client.close()
                daemon.shutdown()
        finally:
            for name in ['run', 'etc']:
                subprocess.run(['umount', '/' + name], check=True)


if __name__ == '__main__': main()
