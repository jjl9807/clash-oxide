#!/usr/bin/env python3
"""Exercise the built daemon over real IPC, HTTP and SOCKS, using only loopback."""
import concurrent.futures
import http.server
import json
import os
import re
from pathlib import Path
import select
import signal
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('CLASH_OXIDE_TEST_BINARY', ROOT / 'target/debug/clash-oxide')).resolve()
YAML = """proxies: []
proxy-groups:
  - name: Choice
    type: select
    proxies: [DIRECT, REJECT]
rules: ['MATCH,Choice']
"""

def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def eventually(check, timeout=15):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, AssertionError) as error:
            last = error
        time.sleep(.1)
    raise AssertionError(f'timed out: {last}')

def terminal_title(output):
    # Ratatui sends changed cells, so status text is often split across cursor moves.
    title = [' '] * 160
    row = column = 1
    for token in re.split(r'(\x1b\[[0-?]*[ -/]*[@-~])', bytes(output).decode('utf-8', errors='replace')):
        if token.startswith('\x1b['):
            if token[-1:] in ('H', 'f'):
                fields = token[2:-1].split(';')
                row = int(fields[0] or 1)
                column = int(fields[1] or 1) if len(fields) > 1 else 1
            continue
        for character in token:
            if character == '\r': column = 1
            elif character == '\n': row += 1
            else:
                width = 0 if unicodedata.combining(character) else 2 if unicodedata.east_asian_width(character) in ('W', 'F') else 1
                if row == 1 and 1 <= column <= len(title):
                    title[column - 1] = character
                    if width == 2 and column < len(title): title[column] = ''
                column += width
    return ''.join(title)

class Daemon:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.sock = self.directory / 'control.sock'
        self.data = self.directory / 'data'
        self.port = free_port()
        self.log = open(self.directory / 'daemon.log', 'a+')
        self.process = None

    def seed(self):
        # Isolated instances must never contend for the user's default port.
        state = self.data / 'state.json'
        if not state.exists():
            self.data.mkdir(parents=True, exist_ok=True, mode=0o700)
            state.write_text(json.dumps({'profiles': [], 'active': None, 'settings': {
                'mixed_port': self.port, 'tun': False, 'system_proxy': False, 'mode': 'rule'}}))
            state.chmod(0o600)

    def start(self):
        self.seed()
        self.process = subprocess.Popen([str(BINARY), 'daemon',
            '--socket', str(self.sock), '--data-dir', str(self.data)], stdout=self.log, stderr=self.log)
        eventually(lambda: self.rpc()['phase'] in ('Running', 'Failed'))

    def rpc(self, command=None, version=2, error=False):
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(120)
            sock.connect(str(self.sock))
            request = {'version': version, 'id': 7, 'request': 'Snapshot' if command is None else {'Command': command}}
            sock.sendall(json.dumps(request).encode() + b'\n')
            reply = json.loads(sock.makefile('rb').readline())
            assert reply['id'] == 7
            if error:
                assert reply['error'], reply
                return reply['error']
            assert not reply['error'], reply['error']
            return reply['snapshot']

    def stop(self, kill=False):
        if self.process and self.process.poll() is None:
            self.process.send_signal(signal.SIGKILL if kill else signal.SIGTERM)
            code = self.process.wait(timeout=15)
            if not kill:
                assert code == 0, self.logs()

    def logs(self):
        self.log.flush()
        return (self.directory / 'daemon.log').read_text()

    def shutdown(self):
        result = subprocess.run([str(BINARY), 'kill', '--socket', str(self.sock),
            '--data-dir', str(self.data)], capture_output=True, text=True, timeout=160)
        assert result.returncode == 0, result.stderr
        if self.process and self.process.poll() is None:
            assert self.process.wait(timeout=5) == 0, self.logs()
        assert not self.sock.exists()

    def configure(self, path):
        self.rpc({'SetMixedPort': self.port})
        return self.rpc({'Import': {'name': 'Local demo', 'source': str(path)}})['active_profile']

def socks_connect(port, target_port):
    sock = socket.create_connection(('127.0.0.1', port), timeout=5)
    sock.sendall(b'\x05\x01\x00')
    assert sock.recv(2) == b'\x05\x00'
    sock.sendall(b'\x05\x01\x00\x01' + socket.inet_aton('127.0.0.1') + struct.pack('!H', target_port))
    response = sock.recv(64)
    assert response[:2] == b'\x05\x00', response
    return sock

def http_get(proxy, target, path='/ok', host='127.0.0.1'):
    with socket.create_connection(('127.0.0.1', proxy), timeout=5) as sock:
        sock.sendall(f'GET http://{host}:{target}{path} HTTP/1.1\r\nHost: {host}:{target}\r\nConnection: close\r\n\r\n'.encode())
        data = b''
        while chunk := sock.recv(8192):
            data += chunk
        return data

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = YAML.encode() if self.path == '/profile' else b'oxide proxy works'
        if self.path == '/invalid': body = b'not: [valid'
        if self.path == '/large': body = b'x' * (2 * 1024 * 1024 + 1)
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try: self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError): pass
    def log_message(self, *_): pass

def main():
    with tempfile.TemporaryDirectory(prefix='oxide-smoke-') as directory:
        daemon = Daemon(directory)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        target = server.server_address[1]
        profile = Path(directory) / 'source.yaml'
        profile.write_text(YAML)
        try:
            daemon.start()
            assert 'version mismatch' in daemon.rpc(version=99, error=True)
            assert daemon.rpc()['phase'] == 'Running'
            assert daemon.rpc()['active_profile'] is None
            local = daemon.configure(profile)
            assert len(daemon.rpc()['profiles']) == 1
            assert daemon.rpc()['phase'] == 'Running'
            assert b'oxide proxy works' in http_get(daemon.port, target)
            with socks_connect(daemon.port, target) as sock:
                sock.sendall(b'GET /ok HTTP/1.0\r\n\r\n')
                assert b'200 OK' in sock.recv(4096)
            with socks_connect(daemon.port, target) as held:
                def has_connections():
                    state = daemon.rpc()
                    return state if state['engine']['connection_count'] else None
                state = eventually(has_connections)
                assert state['engine']['traffic']['download_total'] > 0
                connection = next(c for c in state['engine']['connections'] if str(target) in c['destination'])
                daemon.rpc({'CloseConnection': {'id': connection['id']}})
                assert held.recv(1) == b''
            with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
                assert all(s['phase'] == 'Running' for s in pool.map(lambda _: daemon.rpc(), range(12)))
            daemon.rpc({'SelectProxy': {'group': 'Choice', 'proxy': 'REJECT'}})
            rejected = http_get(daemon.port, target)
            assert b'oxide proxy works' not in rejected
            daemon.rpc('Reload')
            assert next(g for g in daemon.rpc()['engine']['groups'] if g['name'] == 'Choice')['selected'] == 'REJECT'
            daemon.rpc({'SelectProxy': {'group': 'Choice', 'proxy': 'DIRECT'}})
            # A failed replacement must restore the existing working listener.
            with socket.socket() as occupied:
                occupied.bind(('127.0.0.1', 0)); occupied.listen()
                assert 'already in use' in daemon.rpc({'SetMixedPort': occupied.getsockname()[1]}, error=True)
            assert daemon.rpc()['settings']['mixed_port'] == daemon.port
            assert b'oxide proxy works' in http_get(daemon.port, target)
            # This passes YAML validation and fails inside create_components,
            # exercising cleanup of the failed runtime before rollback.
            (daemon.data / 'profiles' / local / 'broken.mmdb').write_bytes(b'invalid geodata')
            profile.write_text(YAML + f'mmdb: broken.mmdb\nmmdb-download-url: http://127.0.0.1:{target}/invalid\n')
            daemon.rpc({'Refresh': {'id': local}}, error=True)
            assert daemon.rpc()['phase'] == 'Running'
            assert b'oxide proxy works' in http_get(daemon.port, target)
            profile.write_text('not: [valid')
            daemon.rpc({'Refresh': {'id': local}}, error=True)
            assert b'oxide proxy works' in http_get(daemon.port, target)
            profile.write_text(YAML)
            daemon.rpc({'Refresh': {'id': local}})
            state = daemon.rpc({'Import': {'name': 'Subscription', 'source': f'http://127.0.0.1:{target}/profile'}})
            remote = next(p['id'] for p in state['profiles'] if p['subscription'])
            for path in ['/invalid', '/large']:
                daemon.rpc({'Import': {'name': 'Invalid', 'source': f'http://127.0.0.1:{target}{path}'}}, error=True)
            assert len(daemon.rpc()['profiles']) == 2
            daemon.rpc({'SwitchProfile': {'id': remote}})
            daemon.rpc({'Refresh': {'id': remote}})
            daemon.rpc({'RemoveProfile': {'id': local}})
            assert b'oxide proxy works' in http_get(daemon.port, target)
            daemon.stop()
            daemon.start()
            eventually(lambda: daemon.rpc()['phase'] == 'Running')
            assert daemon.rpc()['active_profile'] == remote
            assert b'oxide proxy works' in http_get(daemon.port, target)
            daemon.stop(kill=True)
            daemon.start()
            eventually(lambda: daemon.rpc()['phase'] == 'Running')
            assert b'oxide proxy works' in http_get(daemon.port, target)
            daemon.shutdown()
            with socket.socket() as available:
                available.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                available.bind(('127.0.0.1', daemon.port))
            daemon.stop(); daemon.start()
            assert daemon.rpc()['phase'] == 'Running'
            assert daemon.sock.stat().st_mode & 0o777 == 0o600
            assert (daemon.data / 'state.json').stat().st_mode & 0o777 == 0o600
            daemon.shutdown()
            daemon.shutdown()  # Already stopped is a successful no-op.
            print('PASS: local/URL import, HTTP + SOCKS, stats, connection close, node persistence, rollback, refresh, concurrent IPC, graceful/crash restart, private storage')
        except Exception:
            print(daemon.logs()[-12000:])
            raise
        finally:
            daemon.stop()
            server.shutdown()

if __name__ == '__main__': main()
