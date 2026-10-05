#!/usr/bin/env python3
"""Test real GSettings in a PRIVATE D-Bus session and temporary dconf directory."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('smoke-test.py'))
smoke = importlib.util.module_from_spec(spec); spec.loader.exec_module(smoke)

def get(schema, key):
    return subprocess.check_output(['gsettings', 'get', schema, key], text=True).strip()

def main(directory):
    assert os.environ.get('OXIDE_PRIVATE_DBUS_TEST') == directory
    daemon = smoke.Daemon(directory)
    profile = Path(directory) / 'source.yaml'; profile.write_text(smoke.YAML)
    schema = 'org.gnome.system.proxy'
    baseline = {(s, k): get(s, k) for s, keys in [(schema, ['mode', 'use-same-proxy'])] + [(schema + '.' + protocol, ['host', 'port']) for protocol in ['http', 'https', 'socks']] for k in keys}
    try:
        daemon.start(); daemon.configure(profile)
        assert daemon.rpc({'SetSystemProxy': True})['system_proxy_active']
        assert get(schema, 'mode') == "'manual'"
        assert get(schema + '.http', 'port') == str(daemon.port)
        daemon.shutdown()
        assert all(get(s, k) == v for (s, k), v in baseline.items())
        daemon.start(); smoke.eventually(lambda: daemon.rpc()['system_proxy_active'])
        daemon.rpc({'SetSystemProxy': False})
        assert all(get(s, k) == v for (s, k), v in baseline.items())
        assert daemon.rpc()['phase'] == 'Running'
        daemon.rpc({'SetSystemProxy': True}); daemon.stop(kill=True)
        daemon.start(); smoke.eventually(lambda: daemon.rpc()['system_proxy_active'])
        daemon.rpc({'SetSystemProxy': False})
        assert all(get(s, k) == v for (s, k), v in baseline.items())
        daemon.rpc({'SetSystemProxy': True})
        subprocess.run(['gsettings', 'set', schema, 'mode', 'auto'], check=True)
        daemon.rpc({'SetSystemProxy': False})
        assert get(schema, 'mode') == "'auto'", 'Must preserve settings changed by another application'
        print('PASS: GNOME proxy apply, restore, crash recovery and external-change preservation (private D-Bus/dconf)')
    except Exception:
        print(daemon.logs()[-10000:]); raise
    finally: daemon.stop()

if __name__ == '__main__':
    if len(sys.argv) == 2: main(sys.argv[1])
    else:
        with tempfile.TemporaryDirectory(prefix='oxide-dconf-') as directory:
            env = os.environ.copy()
            env['XDG_CONFIG_HOME'] = directory
            env['OXIDE_PRIVATE_DBUS_TEST'] = directory
            env.pop('DCONF_PROFILE', None); env.pop('GSETTINGS_BACKEND', None)
            subprocess.run(['dbus-run-session', '--', sys.executable, __file__, directory], env=env, check=True)
