#!/usr/bin/env python3
"""Test unified entry points, real TUI auto-start, isolation, and graceful kill."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('smoke-test.py'))
smoke = importlib.util.module_from_spec(spec); spec.loader.exec_module(smoke)


class Tui:
    def __init__(self, daemon, cwd=None, relative=False, managed=False):
        self.master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
        env = os.environ.copy()
        env.pop('CLASH_OXIDE_SOCKET', None)
        env['TERM'] = 'xterm-256color'
        env['RUST_LOG'] = 'info'
        def terminal():
            os.setsid(); fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        args = [str(smoke.BINARY), 'tui', '--data-dir', 'data' if relative else str(daemon.data)]
        if not relative: args += ['--socket', str(daemon.sock)]
        if managed: args = [str(smoke.BINARY), 'tui']
        self.process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave,
            env=env, cwd=cwd, preexec_fn=terminal)
        os.close(slave)
        self.output = bytearray()
        def drain():
            try:
                while data := os.read(self.master, 65536): self.output.extend(data)
            except OSError: pass
        self.thread = threading.Thread(target=drain, daemon=True); self.thread.start()

    def close(self):
        if self.process.poll() is None:
            os.write(self.master, b'\x03')
            try: assert self.process.wait(timeout=5) == 0
            except subprocess.TimeoutExpired:
                self.process.terminate(); self.process.wait(timeout=5)
                raise
        os.close(self.master)


def peer_pid(path):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(str(path))
        return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[0]


def cli(daemon, *args):
    return subprocess.run([str(smoke.BINARY), *args, '--socket', str(daemon.sock),
        '--data-dir', str(daemon.data)], capture_output=True, text=True, timeout=160)


def main():
    with tempfile.TemporaryDirectory(prefix='oxide-lifecycle-') as directory:
        daemon = smoke.Daemon(directory)
        clients = []
        try:
            # Read-only CLI and noninteractive TUI must not create a daemon.
            assert cli(daemon, 'ctl', 'status').returncode != 0
            assert 'not running' in cli(daemon, 'kill').stdout
            assert 'interactive terminal' in cli(daemon, 'tui').stderr
            assert not daemon.data.exists()
            # A stale socket must not be mistaken for a live daemon.
            with socket.socket(socket.AF_UNIX) as stale: stale.bind(str(daemon.sock))
            daemon.seed()
            clients = [Tui(daemon), Tui(daemon)]
            smoke.eventually(lambda: daemon.rpc()['revision'] > 0)
            for client in clients:
                smoke.eventually(lambda: 'Daemon connected' in smoke.terminal_title(client.output))
            pid = peer_pid(daemon.sock)
            assert os.getsid(pid) == pid, 'Auto-started daemon must be detached from the terminal'
            assert (daemon.data / 'daemon.log').stat().st_mode & 0o777 == 0o600
            assert (daemon.data / 'daemon.log').read_text().count('Daemon ready') == 1
            source = Path(directory) / 'source.yaml'; source.write_text(smoke.YAML)
            daemon.configure(source)
            assert json.loads(cli(daemon, 'ctl', 'status').stdout)['phase'] == 'Running'
            # There is no independent core stop/start; reload retains the daemon.
            assert cli(daemon, 'ctl', 'start').returncode != 0
            assert cli(daemon, 'ctl', 'stop').returncode != 0
            assert cli(daemon, 'ctl', 'reload').returncode == 0
            assert peer_pid(daemon.sock) == pid
            daemon.shutdown()
            smoke.eventually(lambda: not Path(f'/proc/{pid}').exists())
            with socket.socket() as available: available.bind(('127.0.0.1', daemon.port))
            time.sleep(3)
            assert not daemon.sock.exists(), 'Existing frontends must not undo kill'
            assert all(c.process.poll() is None for c in clients)
            # Opening a new frontend explicitly starts the daemon again.
            clients.append(Tui(daemon))
            smoke.eventually(lambda: daemon.rpc()['phase'] == 'Running')
            for client in clients: client.close()
            clients.clear()
            assert daemon.rpc()['phase'] == 'Running', 'Daemon must outlive all frontends'
            daemon.shutdown()
            assert 'not running' in cli(daemon, 'kill').stdout
            # --data-dir alone derives an isolated socket, and relative paths survive chdir("/").
            daemon.sock = daemon.data / 'control.sock'
            clients.append(Tui(daemon, cwd=directory, relative=True))
            smoke.eventually(lambda: daemon.rpc()['phase'] == 'Running')
            clients.pop().close()
            daemon.shutdown()
            print('PASS: one CLI, concurrent auto-start, stale socket, detached lifetime, automatic core, reload vs kill, no unwanted respawn, configuration restore, relative/custom paths')
        finally:
            for client in clients: client.close()
            daemon.shutdown()

    # An incompatible listener must produce an error without spawning a second daemon.
    with tempfile.TemporaryDirectory(prefix='oxide-incompatible-') as directory:
        daemon = smoke.Daemon(directory)
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(daemon.sock)); listener.listen(); listener.settimeout(.2)
            done = threading.Event()
            def reject():
                while not done.is_set():
                    try: stream, _ = listener.accept()
                    except socket.timeout: continue
                    with stream:
                        data = json.loads(stream.makefile('rb').readline())
                        stream.sendall(json.dumps({'version': 99, 'id': data['id'], 'snapshot': None,
                            'error': 'Protocol version mismatch'}).encode() + b'\n')
            thread = threading.Thread(target=reject); thread.start()
            client = Tui(daemon)
            try:
                smoke.eventually(lambda: b'handshake failed' in client.output)
                assert not daemon.data.exists()
                assert cli(daemon, 'kill').returncode != 0
            finally:
                client.close(); done.set(); thread.join(timeout=3)
        print('PASS: incompatible daemon is reported without replacement or process-name killing')


if __name__ == '__main__': main()
