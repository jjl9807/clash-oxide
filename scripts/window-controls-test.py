#!/usr/bin/env python3
"""Click real GPUI title-bar controls under private Xvfb + Xfwm + D-Bus."""
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('smoke-test.py'))
smoke = importlib.util.module_from_spec(spec); spec.loader.exec_module(smoke)
OUT = smoke.ROOT / 'target/ui-smoke'


def main(directory):
    base = Path(directory)
    assert os.environ.get('OXIDE_PRIVATE_WINDOW_TEST') == directory
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None); env.pop('SESSION_MANAGER', None)
    env['GDK_BACKEND'] = 'x11'
    daemon = smoke.Daemon(base)
    children = []
    OUT.mkdir(parents=True, exist_ok=True)
    def spawn(args, name, **kwargs):
        process = subprocess.Popen(args, env=env, stderr=open(OUT / (name + '.log'), 'w'), **kwargs)
        children.append(process)
        return process
    try:
        xvfb = spawn(['Xvfb', '-displayfd', '1', '-screen', '0', '1440x1000x24', '-nolisten', 'tcp'],
            'controls-xvfb', stdout=subprocess.PIPE, text=True)
        number = xvfb.stdout.readline().strip(); assert number.isdigit()
        env['DISPLAY'] = ':' + number
        # Installed Xfconf is D-Bus activated; allow a temporary extracted binary in CI.
        if conf := env.get('CLASH_OXIDE_TEST_XFCONFD'):
            spawn([conf], 'controls-xfconf')
            time.sleep(.3)
        spawn(['xfwm4', '--compositor=on', '--sm-client-disable'], 'controls-wm')
        def command(*args):
            return subprocess.check_output(args, env=env, text=True, stderr=subprocess.DEVNULL)
        smoke.eventually(lambda: '_GTK_FRAME_EXTENTS' in command('xprop', '-root', '_NET_SUPPORTED'))
        source = base / 'profile.yaml'; source.write_text(smoke.YAML)
        daemon.start(); daemon.configure(source)
        app = spawn([str(smoke.BINARY), 'gui', '--socket', str(daemon.sock), '--data-dir', str(daemon.data)],
            'controls-gui', stdout=subprocess.DEVNULL)
        def find_window():
            try: return command('xdotool', 'search', '--onlyvisible', '--name', '^Clash Oxide$').splitlines()[0]
            except (subprocess.CalledProcessError, IndexError): return None
        window = smoke.eventually(find_window)
        command('xdotool', 'windowactivate', '--sync', window)
        def geometry():
            return {key: int(value) for key, value in re.findall(r'^(X|Y|WIDTH|HEIGHT)=(-?\d+)$',
                command('xdotool', 'getwindowgeometry', '--shell', window), re.M)}
        def state(): return command('xprop', '-id', window, '_NET_WM_STATE')
        def frame():
            values = command('xprop', '-id', window, '_GTK_FRAME_EXTENTS')
            assert '=' in values, 'GPUI must use client decorations for this test'
            return [int(v) for v in values.split('=', 1)[1].split(',')]
        smoke.eventually(lambda: '=' in command('xprop', '-id', window, '_GTK_FRAME_EXTENTS'))
        time.sleep(1)
        def click_control(index):
            bounds = geometry(); left, right, top, bottom = frame()
            # The TitleBar component has 34px controls, inside the 1px client frame.
            command('xdotool', 'mousemove', '--window', window,
                str(bounds['WIDTH'] - right - 1 - 17 - index * 34), str(top + 1 + 17), 'click', '1')
        subprocess.run(['import', '-display', env['DISPLAY'], '-window', window,
            str(OUT / 'window-controls.png')], env=env, check=True)
        click_control(1)
        smoke.eventually(lambda: '_NET_WM_STATE_MAXIMIZED_VERT' in state())
        click_control(1)
        smoke.eventually(lambda: '_NET_WM_STATE_MAXIMIZED_VERT' not in state())
        click_control(2)
        smoke.eventually(lambda: '_NET_WM_STATE_HIDDEN' in state())
        command('xdotool', 'windowactivate', '--sync', window)
        smoke.eventually(lambda: '_NET_WM_STATE_HIDDEN' not in state())
        time.sleep(.5)
        # Double clicking the blank title-bar area maximizes and restores the window.
        for maximized in [True, False]:
            left, right, top, bottom = frame()
            command('xdotool', 'mousemove', '--window', window, '350', str(top + 17),
                'click', '--repeat', '2', '--delay', '100', '1')
            smoke.eventually(lambda: ('_NET_WM_STATE_MAXIMIZED_VERT' in state()) == maximized)
            time.sleep(.5)
        before = geometry()
        left, right, top, bottom = frame()
        command('xdotool', 'mousemove', '--window', window, '350', str(top + 17), 'mousedown', '1')
        command('xdotool', 'mousemove_relative', '--sync', '10', '5')
        time.sleep(.2)
        command('xdotool', 'mousemove_relative', '--sync', '40', '20')
        command('xdotool', 'mouseup', '1')
        smoke.eventually(lambda: (geometry()['X'], geometry()['Y']) != (before['X'], before['Y']))
        click_control(0)
        assert app.wait(timeout=8) == 0
        assert daemon.rpc()['phase'] == 'Running', 'Closing the window must leave the daemon running'
        print('PASS: real minimize/maximize/restore/close buttons, double click, title-bar drag; daemon survives GUI close (private Xfwm/Xvfb)')
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate(); child.wait(timeout=8)
        daemon.shutdown(); daemon.stop()


if __name__ == '__main__':
    if len(sys.argv) == 2: main(sys.argv[1])
    else:
        with tempfile.TemporaryDirectory(prefix='oxide-window-controls-') as directory:
            env = os.environ.copy()
            env['OXIDE_PRIVATE_WINDOW_TEST'] = directory
            env['XDG_CONFIG_HOME'] = str(Path(directory) / 'config')
            env['XDG_DATA_HOME'] = str(Path(directory) / 'data')
            runtime = Path(directory) / 'runtime'; runtime.mkdir(mode=0o700)
            env['XDG_RUNTIME_DIR'] = str(runtime)
            env['GIO_USE_VFS'] = 'local'
            env['NO_AT_BRIDGE'] = '1'
            # Activate only Xfconf; desktop portals can otherwise create FUSE mounts in the test runtime.
            services = Path(directory) / 'services'; services.mkdir()
            for data_dir in env.get('XDG_DATA_DIRS', '/usr/local/share:/usr/share').split(':'):
                service = Path(data_dir) / 'dbus-1/services/org.xfce.Xfconf.service'
                if service.exists():
                    shutil.copy(service, services / service.name)
                    break
            bus = Path(directory) / 'bus.conf'
            bus.write_text(f'''<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<servicedir>{services}</servicedir><policy context="default">
<allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/>
</policy></busconfig>''')
            subprocess.run(['dbus-run-session', '--config-file', str(bus), '--', sys.executable,
                __file__, directory], env=env, check=True)
