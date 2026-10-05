#!/usr/bin/env python3
"""Exercise automatic core startup, failed imports and recovery."""
import http.server
import importlib.util
from pathlib import Path
import socket
import tempfile
import threading

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('smoke-test.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def main():
    with tempfile.TemporaryDirectory(prefix='oxide-always-on-') as directory:
        daemon = smoke.Daemon(directory)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), smoke.Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        source = Path(directory) / 'source.yaml'
        source.write_text(smoke.YAML)
        target = server.server_port
        try:
            daemon.start()
            state = daemon.rpc()
            assert state['phase'] == 'Running' and state['active_profile'] is None
            assert not state['profiles'] and not state['tun_active'] and not state['system_proxy_active']
            assert state['engine']['rules'][0]['target'] == 'DIRECT'
            assert b'oxide proxy works' in smoke.http_get(daemon.port, target)
            assert 'version mismatch' in daemon.rpc(version=99, error=True)

            # Valid YAML can still fail inside the core. A first import must not
            # leave a selected but unusable profile or destroy the default core.
            source.write_text(smoke.YAML + f'mmdb: broken.mmdb\nmmdb-download-url: http://127.0.0.1:{target}/invalid\n')
            daemon.rpc({'Import': {'name': 'Broken first import', 'source': str(source)}}, error=True)
            state = daemon.rpc()
            assert state['phase'] == 'Running' and state['active_profile'] is None
            assert not state['profiles'] and not list((daemon.data / 'profiles').glob('*.yaml'))
            assert source.exists()
            assert b'oxide proxy works' in smoke.http_get(daemon.port, target)

            source.write_text(smoke.YAML)
            state = daemon.rpc({'Import': {'name': 'First import', 'source': str(source)}})
            first = state['active_profile']
            assert first and state['phase'] == 'Running'
            assert any(g['name'] == 'Choice' for g in state['engine']['groups'])
            state = daemon.rpc({'Import': {'name': 'Second import', 'source': str(source)}})
            second = next(p['id'] for p in state['profiles'] if p['id'] != first)
            assert state['active_profile'] == first

            # Restart automatically restores the selected configuration.
            daemon.shutdown()
            daemon.start()
            assert daemon.rpc()['phase'] == 'Running'
            assert daemon.rpc()['active_profile'] == first

            # A broken saved configuration stays visibly failed, with IPC alive.
            # Repair + reload, or selecting another valid profile, recovers it.
            daemon.shutdown()
            managed = daemon.data / 'profiles' / (first + '.yaml')
            managed.write_text('not: [valid')
            daemon.start()
            state = daemon.rpc()
            assert state['phase'] == 'Failed' and state['last_diagnostic']
            assert state['active_profile'] == first and not state['engine']['proxies']
            managed.write_text(smoke.YAML)
            assert daemon.rpc('Reload')['phase'] == 'Running'
            daemon.shutdown()
            managed.write_text('not: [valid')
            daemon.start()
            state = daemon.rpc({'SwitchProfile': {'id': second}})
            assert state['phase'] == 'Running' and state['active_profile'] == second
            assert b'oxide proxy works' in smoke.http_get(daemon.port, target)

            # An occupied listening port is recoverable from the settings UI.
            daemon.shutdown()
            with socket.socket() as occupied:
                occupied.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                occupied.bind(('127.0.0.1', daemon.port))
                occupied.listen()
                daemon.start()
                assert daemon.rpc()['phase'] == 'Failed'
                daemon.port = smoke.free_port()
                assert daemon.rpc({'SetMixedPort': daemon.port})['phase'] == 'Running'
            assert b'oxide proxy works' in smoke.http_get(daemon.port, target)
            daemon.shutdown()
            with socket.socket() as released:
                released.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                released.bind(('127.0.0.1', daemon.port))
            print('PASS: default core, first-import activation/rollback, selected configuration restore, startup failures, reload/profile/port recovery, shutdown cleanup')
        except Exception:
            print(daemon.logs()[-10000:])
            raise
        finally:
            daemon.stop()
            server.shutdown()
            server.server_close()


if __name__ == '__main__':
    main()
