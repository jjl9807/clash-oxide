#!/usr/bin/env python3
"""Check Adwaita rendering and theme/desktop behavior on private D-Bus/Xvfb."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

PORTAL = 'org.freedesktop.portal.Desktop'
SETTINGS = 'org.freedesktop.portal.Settings'
FIXTURE = 'org.clash_oxide.ThemeTest'


def serve_portal():
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName(PORTAL, bus, do_not_queue=True)

    class Portal(dbus.service.Object):
        def __init__(self):
            super().__init__(name, '/org/freedesktop/portal/desktop')
            self.scheme = 2
            self.reads = 0

        @dbus.service.method(SETTINGS, in_signature='ss', out_signature='v')
        def Read(self, namespace, key):
            if namespace == 'org.freedesktop.appearance':
                if key == 'color-scheme':
                    self.reads += 1
                    return dbus.UInt32(self.scheme, variant_level=1)
                return dbus.UInt32(0, variant_level=1)
            if key == 'cursor-theme': return dbus.String('Adwaita', variant_level=1)
            if key == 'cursor-size': return dbus.Int32(24, variant_level=1)
            if key == 'button-layout': return dbus.String(':minimize,maximize,close', variant_level=1)
            raise dbus.exceptions.DBusException('Unknown setting')

        @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='ss', out_signature='v')
        def Get(self, interface, prop):
            assert interface == SETTINGS and prop == 'version'
            return dbus.UInt32(2, variant_level=1)

        @dbus.service.method(FIXTURE, in_signature='u', out_signature='')
        def SetScheme(self, value):
            self.scheme = int(value)
            self.SettingChanged('org.freedesktop.appearance', 'color-scheme', dbus.UInt32(value))

        @dbus.service.method(FIXTURE, in_signature='', out_signature='u')
        def Reads(self): return self.reads

        @dbus.service.signal(SETTINGS, signature='ssv')
        def SettingChanged(self, namespace, key, value): pass

    portal = Portal()
    GLib.MainLoop().run()


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test(directory):
    assert os.environ.get('OXIDE_PRIVATE_THEME_TEST') == directory
    smoke = load_script('smoke-test')
    ui = load_script('ui-smoke-test')
    base = Path(directory)
    out = smoke.ROOT / 'target/gui-theme'
    out.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env['CLASH_OXIDE_LANG'] = 'en'
    children = []
    daemon = smoke.Daemon(base)

    def spawn(args, label, **kwargs):
        process = subprocess.Popen(args, env=env, stderr=open(out / (label + '.log'), 'w'), **kwargs)
        children.append(process)
        return process

    def command(*args):
        return subprocess.check_output(args, env=env, stderr=subprocess.DEVNULL)

    def window_id():
        try: return command('xdotool', 'search', '--onlyvisible', '--name', '^Clash Oxide$').decode().splitlines()[0]
        except subprocess.CalledProcessError: return None

    def key(*args):
        command('xdotool', 'key', '--clearmodifiers', *args)
        time.sleep(.25)

    def click(x, y):
        command('xdotool', 'mousemove', '--window', window, str(x), str(y), 'click', '1')
        command('xdotool', 'mousemove', '--window', window, '1080', '700')
        time.sleep(.25)

    def shot(name):
        path = out / (name + '.png')
        subprocess.run(['import', '-display', env['DISPLAY'], '-window', window, str(path)], env=env, check=True)
        return path

    def color(path, x, y):
        return tuple(command('convert', str(path), '-crop', f'1x1+{x}+{y}', '-depth', '8', 'rgb:-'))

    def has_mode(mode):
        pixel = color(shot('current'), 1080, 650)
        expected = (34,34,38) if mode == 'dark' else (250,250,251)
        return all(abs(a-b) <= 2 for a,b in zip(pixel, expected))

    def wait_mode(mode): smoke.eventually(lambda: has_mode(mode))

    def start_gui():
        app = spawn([str(smoke.BINARY), 'gui', '--socket', str(daemon.sock), '--data-dir', str(daemon.data)], 'gui', stdout=subprocess.DEVNULL)
        window = smoke.eventually(window_id)
        command('xdotool', 'windowfocus', '--sync', window)
        time.sleep(.6)
        return app, window

    def capture_pages(mode):
        for index, page in enumerate(['proxies', 'profiles', 'connections', 'rules', 'logs', 'settings'], 1):
            key('ctrl+' + str(index))
            shot(mode + '-' + page)
        frame = out / (mode + '-proxies.png')
        surface = (52,52,55) if mode == 'dark' else (255,255,255)
        selection = (52,88,127) if mode == 'dark' else (219,233,250)
        blue = (53,132,228)
        assert color(frame, 280, 125) == surface, f'{mode}: group card must have its own surface'
        assert color(frame, 280, 235) == selection, f'{mode}: selected nodes must use a tinted surface'
        assert color(frame, 20, 250) == blue, f'{mode}: navigation must use Adwaita accent blue'
        assert color(frame, 45, 20) == blue, f'{mode}: selected mode buttons must match navigation'
        settings = out / (mode + '-settings.png')
        selected_x = 1035 if mode == 'dark' else 980
        assert color(settings, selected_x, 235) == blue, f'{mode}: selected theme buttons must match navigation'
        assert color(settings, 935, 140) == blue, f'{mode}: selected language buttons must match navigation'
        key('ctrl+2')
        click(795, 310)
        dialog = shot(mode + '-rename-dialog')
        expected = (54,54,58) if mode == 'dark' else (255,255,255)
        assert color(dialog, 335, 150) == expected, f'{mode}: dialogs must use the elevated surface'
        key('Escape')
        click(865, 310)
        shot(mode + '-delete-dialog')
        key('Escape')
        key('ctrl+6')

    try:
        portal_process = spawn([sys.executable, __file__, '--portal'], 'portal', stdout=subprocess.DEVNULL)
        bus = dbus.SessionBus()
        smoke.eventually(lambda: bus.name_has_owner(PORTAL))
        portal = dbus.Interface(bus.get_object(PORTAL, '/org/freedesktop/portal/desktop'), FIXTURE)
        xvfb = spawn(['Xvfb', '-displayfd', '1', '-screen', '0', '1440x1000x24', '-nolisten', 'tcp'], 'xvfb', stdout=subprocess.PIPE, text=True)
        number = xvfb.stdout.readline().strip()
        assert number.isdigit()
        env['DISPLAY'] = ':' + number
        preferences = Path(env['XDG_CONFIG_HOME']) / 'clash-oxide'
        preferences.mkdir(parents=True)
        language = {'language': 'en'}
        view = {'test_url': 'https://example.com/probe', 'sort': 'Name', 'expanded': ['Choice']}
        (preferences / 'frontend.json').write_text(json.dumps(language))
        (preferences / 'view.json').write_text(json.dumps(view))
        saved = preferences / 'gui.json'
        source = base / 'profile.yaml'
        # Populate cards and scrollable lists without contacting external hosts.
        names = [f'🇭🇰 香港 {index:02d}' for index in range(1, 13)] + [f'🇯🇵 日本 {index:02d}' for index in range(1, 13)]
        proxies = [{'name': name, 'type': 'socks5', 'server': '127.0.0.1', 'port': 9} for name in names]
        source.write_text(json.dumps({
            'proxies': proxies,
            'proxy-groups': [{'name': 'Choice', 'type': 'select', 'proxies': ['DIRECT', 'REJECT'] + names}],
            'rules': [f'DOMAIN,example-{index}.test,Choice' for index in range(30)] + ['MATCH,Choice'],
        }, ensure_ascii=False))
        daemon.start()
        daemon.configure(source)
        daemon.rpc({'Import': {'name': 'Second profile', 'source': str(source)}})
        app, window = start_gui()
        smoke.eventually(lambda: portal.Reads() >= 1)
        wait_mode('light')
        assert not saved.exists(), 'Default follow-system must not write preferences on startup'
        key('ctrl+6')
        shot('settings-auto')
        click(1048, 243)
        wait_mode('dark')
        assert json.loads(saved.read_text())['theme'] == 'dark'
        shot('settings-dark')
        capture_pages('dark')
        portal.SetScheme(1); time.sleep(.4)
        portal.SetScheme(2); time.sleep(.4)
        assert has_mode('dark'), 'An explicit theme must ignore desktop changes'
        ui.close_window(env['DISPLAY'], window)
        assert app.wait(timeout=10) == 0
        app, window = start_gui()
        wait_mode('dark')
        key('ctrl+6')
        click(905, 243)
        wait_mode('light')
        assert json.loads(saved.read_text())['theme'] == 'auto'
        portal.SetScheme(1)
        wait_mode('dark')
        shot('system-dark')
        portal.SetScheme(2)
        wait_mode('light')
        portal.SetScheme(0)
        wait_mode('light')
        click(992, 243)
        assert json.loads(saved.read_text())['theme'] == 'light'
        portal.SetScheme(1); time.sleep(.5)
        assert has_mode('light'), 'Explicit light must stay light on a dark desktop'
        capture_pages('light')
        assert json.loads((preferences / 'frontend.json').read_text()) == language
        assert json.loads((preferences / 'view.json').read_text()) == view
        saved.unlink(); saved.mkdir()
        click(1048, 243)
        assert has_mode('light'), 'A failed save must not change the theme'
        shot('save-error')
        assert color(out / 'save-error.png', 270, 720) != (250,250,251), 'Save failures must remain visible'
        saved.rmdir()
        click(992, 243)
        assert json.loads(saved.read_text())['theme'] == 'light'
        shot('save-recovered')
        assert color(out / 'save-recovered.png', 270, 720) == (250,250,251), 'Successful recovery must clear the error without a success banner'
        ui.close_window(env['DISPLAY'], window)
        assert app.wait(timeout=10) == 0
        app, window = start_gui()
        wait_mode('light')
        key('ctrl+6')
        click(1040, 148)
        smoke.eventually(lambda: json.loads((preferences / 'frontend.json').read_text())['language'] == 'zh-CN')
        assert json.loads(saved.read_text())['theme'] == 'light'
        shot('settings-light-zh')
        command('xdotool', 'windowsize', window, '760', '540')
        time.sleep(.4)
        shot('settings-light-zh-narrow')
        command('xdotool', 'windowsize', window, '1100', '760')
        time.sleep(.4)
        key('ctrl+1')
        shot('light-proxies-zh')
        key('ctrl+6')
        click(1048, 243)
        wait_mode('dark')
        shot('settings-dark-zh')
        key('ctrl+1')
        shot('dark-proxies-zh')
        key('ctrl+6')
        command('xdotool', 'windowsize', window, '760', '540')
        time.sleep(.4)
        shot('settings-dark-zh-narrow')
        print('PASS: Adwaita light/dark pages and dialogs, consistent selected controls, theme choices/restart, live desktop changes, independent preferences and failed-save recovery', flush=True)
        assert daemon.rpc()['phase'] == 'Running'
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate(); child.wait(timeout=10)
        daemon.shutdown(); daemon.stop()


def isolated():
    with tempfile.TemporaryDirectory(prefix='oxide-theme-') as directory:
        env = os.environ.copy()
        env['OXIDE_PRIVATE_THEME_TEST'] = directory
        env['XDG_CONFIG_HOME'] = str(Path(directory) / 'config')
        env['XDG_DATA_HOME'] = str(Path(directory) / 'data')
        runtime = Path(directory) / 'runtime'
        runtime.mkdir(mode=0o700)
        env['XDG_RUNTIME_DIR'] = str(runtime)
        config = Path(directory) / 'bus.conf'
        config.write_text('''<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy>
</busconfig>''')
        subprocess.run(['dbus-run-session', '--config-file', str(config), '--', sys.executable, __file__, directory], env=env, check=True)


if __name__ == '__main__':
    if sys.argv[1:] == ['--portal']: serve_portal()
    elif len(sys.argv) == 2: test(sys.argv[1])
    else: isolated()
