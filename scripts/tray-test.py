#!/usr/bin/env python3
"""Exercise the real SNI icon/menu and GPUI lifecycle on a private D-Bus/Xvfb.

The fixture implements the desktop's StatusNotifierWatcher, not the application.
No host panel, user D-Bus session, or installed daemon is touched.
"""
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

WATCHER = 'org.kde.StatusNotifierWatcher'
ITEM = 'org.kde.StatusNotifierItem'
PROPERTIES = 'org.freedesktop.DBus.Properties'
MENU = 'com.canonical.dbusmenu'


def serve_watcher():
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName(WATCHER, bus, do_not_queue=True)

    class Watcher(dbus.service.Object):
        def __init__(self):
            super().__init__(name, '/StatusNotifierWatcher')
            self.items = []
            self.host = False
            bus.add_signal_receiver(self.owner_changed, signal_name='NameOwnerChanged',
                dbus_interface='org.freedesktop.DBus')

        def owner_changed(self, service, old, new):
            if not new:
                for item in list(self.items):
                    if item.split('/')[0] == service:
                        self.items.remove(item)
                        self.StatusNotifierItemUnregistered(item)

        @dbus.service.method(WATCHER, in_signature='s', out_signature='', sender_keyword='sender')
        def RegisterStatusNotifierItem(self, service, sender=None):
            item = sender + service if service.startswith('/') else service + '/StatusNotifierItem'
            if item not in self.items:
                self.items.append(item)
                self.StatusNotifierItemRegistered(item)

        @dbus.service.method(WATCHER, in_signature='s', out_signature='')
        def RegisterStatusNotifierHost(self, service):
            self.SetHost(True)

        @dbus.service.method('org.clash_oxide.Test', in_signature='b', out_signature='')
        def SetHost(self, enabled):
            self.host = bool(enabled)
            if enabled: self.StatusNotifierHostRegistered()
            else: self.StatusNotifierHostUnregistered()

        @dbus.service.method(PROPERTIES, in_signature='ss', out_signature='v')
        def Get(self, interface, prop):
            return self.GetAll(interface)[prop]

        @dbus.service.method(PROPERTIES, in_signature='s', out_signature='a{sv}')
        def GetAll(self, interface):
            assert interface == WATCHER
            return {'RegisteredStatusNotifierItems': dbus.Array(self.items, signature='s'),
                'IsStatusNotifierHostRegistered': dbus.Boolean(self.host),
                'ProtocolVersion': dbus.Int32(0)}

        @dbus.service.signal(WATCHER, signature='s')
        def StatusNotifierItemRegistered(self, item): pass

        @dbus.service.signal(WATCHER, signature='s')
        def StatusNotifierItemUnregistered(self, item): pass

        @dbus.service.signal(WATCHER, signature='')
        def StatusNotifierHostRegistered(self): pass

        @dbus.service.signal(WATCHER, signature='')
        def StatusNotifierHostUnregistered(self): pass

    watcher = Watcher()
    GLib.MainLoop().run()


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test(directory):
    assert os.environ.get('OXIDE_PRIVATE_TRAY_TEST') == directory
    smoke = load_script('smoke-test')
    ui = load_script('ui-smoke-test')
    out = smoke.ROOT / 'target/ui-smoke'
    out.mkdir(parents=True, exist_ok=True)
    base = Path(directory)
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('SESSION_MANAGER', None)
    env['GDK_BACKEND'] = 'x11'
    env['LC_ALL'] = 'C.UTF-8'
    env.pop('CLASH_OXIDE_LANG', None)
    children = []
    daemon = smoke.Daemon(base)
    bus = dbus.SessionBus()

    def spawn(args, log, **kwargs):
        child = subprocess.Popen(args, env=env, stderr=open(out / (log + '.log'), 'w'), **kwargs)
        children.append(child)
        return child

    def command(*args):
        return subprocess.check_output(args, env=env, text=True, stderr=subprocess.DEVNULL)

    def windows():
        try: return command('xdotool', 'search', '--onlyvisible', '--name', '^Clash Oxide$').splitlines()
        except subprocess.CalledProcessError: return []

    def one_window():
        found = windows()
        assert len(found) <= 1, f'Duplicate windows: {found}'
        return found[0] if found else None

    def start_gui(label, options=None, language=None):
        args = [str(smoke.BINARY), 'gui', '--socket', str(daemon.sock), '--data-dir', str(daemon.data)]
        if language: args += ['--lang', language]
        return spawn(args, label, stdout=subprocess.DEVNULL, **(options or {}))

    def find_item(pid):
        watcher = bus.get_object(WATCHER, '/StatusNotifierWatcher')
        items = watcher.Get(WATCHER, 'RegisteredStatusNotifierItems', dbus_interface=PROPERTIES)
        matches = [item for item in items if str(item).startswith(f'org.kde.StatusNotifierItem-{pid}-')]
        assert len(matches) <= 1, 'Duplicate tray items'
        if matches:
            destination, path = str(matches[0]).split('/', 1)
            return bus.get_object(destination, '/' + path)

    def layout(item):
        path = item.Get(ITEM, 'Menu', dbus_interface=PROPERTIES)
        menu = bus.get_object(item.bus_name, path)
        _, root = menu.GetLayout(0, -1, [], dbus_interface=MENU)
        entries = {}
        def visit(node):
            ident, properties, children = node
            if 'label' in properties: entries[str(properties['label'])] = (ident, properties)
            for child in children: visit(child)
        visit(root)
        return menu, entries

    def choose(item, label):
        def enabled():
            menu, entries = layout(item)
            row = entries.get(label)
            return (menu, row[0]) if row and row[1].get('enabled', True) else None
        menu, ident = smoke.eventually(enabled)
        menu.Event(ident, 'clicked', dbus.Int32(0), dbus.UInt32(0), dbus_interface=MENU)

    def tooltip(item):
        return str(item.Get(ITEM, 'ToolTip', dbus_interface=PROPERTIES)[3]).removeprefix('Clash Oxide — ')

    def close_to_tray(app):
        window = smoke.eventually(one_window)
        ui.close_window(env['DISPLAY'], window)
        smoke.eventually(lambda: not windows())
        time.sleep(.3)
        assert app.poll() is None, 'Closing to a usable tray exited the GUI'

    try:
        xvfb = spawn(['Xvfb', '-displayfd', '1', '-screen', '0', '1440x1000x24', '-nolisten', 'tcp'],
            'tray-xvfb', stdout=subprocess.PIPE, text=True)
        display = xvfb.stdout.readline().strip()
        assert display.isdigit()
        env['DISPLAY'] = ':' + display
        if conf := env.get('CLASH_OXIDE_TEST_XFCONFD'):
            spawn([conf], 'tray-xfconf')
            time.sleep(.3)
        spawn(['xfwm4', '--compositor=on', '--sm-client-disable'], 'tray-wm')
        smoke.eventually(lambda: '_GTK_FRAME_EXTENTS' in command('xprop', '-root', '_NET_SUPPORTED'))
        source = base / 'profile.yaml'
        source.write_text(smoke.YAML)
        daemon.start()
        daemon.configure(source)

        # A successful TrayIconBuilder is insufficient: no watcher means ordinary close.
        app = start_gui('tray-no-watcher')
        window = smoke.eventually(one_window)
        assert '128 x 128' in command('xprop', '-id', window, '_NET_WM_ICON')
        assert 'clash-oxide' in command('xprop', '-id', window, 'WM_CLASS')
        time.sleep(1.5)
        ui.close_window(env['DISPLAY'], window)
        assert app.wait(timeout=10) == 0
        assert daemon.rpc()['phase'] == 'Running'
        print('PASS: PNG window icon, taskbar identity, no-watcher close fallback', flush=True)

        watcher_process = spawn([sys.executable, __file__, '--watcher'], 'tray-watcher')
        smoke.eventually(lambda: bus.name_has_owner(WATCHER))
        control = bus.get_object(WATCHER, '/StatusNotifierWatcher')
        app = start_gui('tray-no-host')
        smoke.eventually(lambda: find_item(app.pid))
        window = smoke.eventually(one_window)
        time.sleep(1.5)
        ui.close_window(env['DISPLAY'], window)
        assert app.wait(timeout=10) == 0, 'A watcher without a host must not hide the window'
        print('PASS: watcher without a registered host also falls back', flush=True)

        control.SetHost(True, dbus_interface='org.clash_oxide.Test')
        # Two concurrent launches must create one window, one icon and one owning process.
        launches = [start_gui('tray-single-' + str(i)) for i in range(2)]
        smoke.eventually(lambda: sum(child.poll() == 0 for child in launches) == 1)
        app = next(child for child in launches if child.poll() is None)
        item = smoke.eventually(lambda: find_item(app.pid))
        window = smoke.eventually(one_window)
        icons = item.Get(ITEM, 'IconPixmap', dbus_interface=PROPERTIES)
        assert any(w == 32 and h == 32 and len(data) == 32 * 32 * 4 for w, h, data in icons)
        smoke.eventually(lambda: tooltip(item) == 'Core running')
        time.sleep(1.2)
        # Real CSD close button: taskbar entry disappears, tray and daemon remain.
        bounds = {k: int(v) for k, v in re.findall(r'^(WIDTH|HEIGHT)=(\d+)$',
            command('xdotool', 'getwindowgeometry', '--shell', window), re.M)}
        frame = command('xprop', '-id', window, '_GTK_FRAME_EXTENTS').split('=', 1)[1]
        left, right, top, bottom = map(int, frame.split(','))
        command('xdotool', 'mousemove', '--window', window,
            str(bounds['WIDTH'] - right - 18), str(top + 18), 'click', '1')
        smoke.eventually(lambda: not windows())
        assert app.poll() is None
        choose(item, 'Global')
        smoke.eventually(lambda: daemon.rpc()['settings']['mode'] == 'global')
        smoke.eventually(lambda: layout(item)[1]['Global'][1].get('toggle-state') == 1)
        assert 'Start proxy' not in layout(item)[1] and 'Stop proxy' not in layout(item)[1]
        assert 'System proxy' in layout(item)[1] and 'Virtual network' in layout(item)[1]
        choose(item, 'Reload')
        smoke.eventually(lambda: daemon.rpc()['phase'] == 'Running')
        smoke.eventually(lambda: tooltip(item) == 'Core running')
        item.Activate(0, 0, dbus_interface=ITEM)
        window = smoke.eventually(one_window)
        command('xdotool', 'windowminimize', window)
        smoke.eventually(lambda: '_NET_WM_STATE_HIDDEN' in command('xprop', '-id', window, '_NET_WM_STATE'))
        choose(item, 'Open Clash Oxide')
        smoke.eventually(lambda: '_NET_WM_STATE_HIDDEN' not in command('xprop', '-id', window, '_NET_WM_STATE'))
        close_to_tray(app)
        duplicate = start_gui('tray-reopen')
        assert duplicate.wait(timeout=10) == 0
        smoke.eventually(one_window)
        assert find_item(app.pid)
        print('PASS: concurrent single instance, CSD close-to-tray, menu actions/state, minimize/restore, activate/reopen', flush=True)

        # Change language via the real Settings button; menus update in-place.
        preferences = Path(env['XDG_CONFIG_HOME']) / 'clash-oxide/frontend.json'
        zh = json.loads((smoke.ROOT / 'crates/i18n/locales/zh-CN.json').read_text())
        pid = load_script('lifecycle-test').peer_pid(daemon.sock)
        window = smoke.eventually(one_window)
        def click(x, y):
            command('xdotool', 'mousemove', '--window', window, str(x), str(y), 'click', '1')
            time.sleep(.3)
        def screenshot(name):
            time.sleep(.3)  # Allow the frame following the tray/activation update to paint.
            command('import', '-display', env['DISPLAY'], '-window', window, str(out / (name + '.png')))
        command('xdotool', 'windowfocus', '--sync', window)
        command('xdotool', 'key', '--clearmodifiers', 'ctrl+6', 'Escape')
        time.sleep(.3)
        screenshot('i18n-settings-before')
        click(1035, 204)
        smoke.eventually(lambda: preferences.exists() and json.loads(preferences.read_text())['language'] == 'zh-CN')
        smoke.eventually(lambda: zh['tray.open'] in layout(item)[1])
        assert tooltip(item) == zh['status.proxy_running']
        screenshot('i18n-settings-zh')
        # Existing-instance activation forwards a temporary language override.
        duplicate = start_gui('i18n-override', language='en')
        assert duplicate.wait(timeout=10) == 0
        smoke.eventually(lambda: 'Open Clash Oxide' in layout(item)[1])
        assert json.loads(preferences.read_text())['language'] == 'zh-CN'
        command('xdotool', 'key', '--clearmodifiers', 'ctrl+2', 'Escape')
        time.sleep(.3)
        screenshot('i18n-profiles-en')
        click(355, 196)
        command('xdotool', 'type', '--clearmodifiers', '--delay', '1', 'Keep this name')
        duplicate = start_gui('i18n-override-zh', language='zh-CN')
        assert duplicate.wait(timeout=10) == 0
        smoke.eventually(lambda: zh['tray.open'] in layout(item)[1])
        screenshot('i18n-profiles-zh')
        # Switching changes placeholders and labels without resetting input state.
        source = base / 'profile.yaml'
        click(650, 196)
        command('xdotool', 'type', '--clearmodifiers', '--delay', '1', str(source))
        command('xdotool', 'key', '--clearmodifiers', 'Return')
        smoke.eventually(lambda: any(p['name'] == 'Keep this name' for p in daemon.rpc()['profiles']))
        assert load_script('lifecycle-test').peer_pid(daemon.sock) == pid
        choose(item, zh['tray.quit'])
        assert app.wait(timeout=10) == 0
        app = start_gui('i18n-persisted')
        item = smoke.eventually(lambda: find_item(app.pid))
        smoke.eventually(lambda: zh['tray.open'] in layout(item)[1])
        window = smoke.eventually(one_window)
        command('xdotool', 'windowfocus', '--sync', window)
        command('xdotool', 'key', '--clearmodifiers', 'ctrl+6', 'Escape')
        time.sleep(.3)
        click(955, 204)  # English, saved for the remaining tests and subsequent launches
        smoke.eventually(lambda: json.loads(preferences.read_text())['language'] == 'en')
        smoke.eventually(lambda: 'Open Clash Oxide' in layout(item)[1])
        print('PASS: GUI live language switch, tray refresh, saved preference, single-instance override, input preservation', flush=True)

        close_to_tray(app)
        daemon.shutdown()
        smoke.eventually(lambda: tooltip(item) == 'Daemon disconnected', timeout=12)
        duplicate = start_gui('tray-reopen-after-kill')
        assert duplicate.wait(timeout=10) == 0
        smoke.eventually(one_window)
        time.sleep(2)
        assert not daemon.sock.exists(), 'Reopening an existing GUI must not revive a killed daemon'
        choose(item, 'Reload')
        smoke.eventually(lambda: daemon.rpc()['phase'] == 'Running')
        print('PASS: kill leaves the daemon stopped; only explicit Reload relaunches it', flush=True)

        close_to_tray(app)
        watcher_process.terminate()
        watcher_process.wait(timeout=5)
        smoke.eventually(one_window)
        assert app.poll() is None, 'Losing the panel must restore the window'
        watcher_process = spawn([sys.executable, __file__, '--watcher'], 'tray-watcher-restarted')
        smoke.eventually(lambda: bus.name_has_owner(WATCHER))
        control = bus.get_object(WATCHER, '/StatusNotifierWatcher')
        control.SetHost(True, dbus_interface='org.clash_oxide.Test')
        item = smoke.eventually(lambda: find_item(app.pid))
        time.sleep(1.5)
        close_to_tray(app)
        print('PASS: watcher loss restores the window; watcher restart re-registers the icon', flush=True)

        choose(item, 'Quit interface (keep proxy running)')
        assert app.wait(timeout=10) == 0
        assert daemon.rpc()['phase'] == 'Running'
        app = start_gui('tray-shutdown')
        item = smoke.eventually(lambda: find_item(app.pid))
        smoke.eventually(lambda: tooltip(item) == 'Core running')
        choose(item, 'Stop proxy and exit')
        assert app.wait(timeout=20) == 0
        assert not daemon.sock.exists(), 'Shutdown must finish before the GUI exits'
        print('PASS: quit-interface preserves the proxy; stop-and-exit waits for daemon shutdown', flush=True)
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
        daemon.shutdown()
        daemon.stop()


def isolated():
    with tempfile.TemporaryDirectory(prefix='oxide-tray-') as directory:
        env = os.environ.copy()
        env['OXIDE_PRIVATE_TRAY_TEST'] = directory
        env['XDG_CONFIG_HOME'] = str(Path(directory) / 'config')
        env['XDG_DATA_HOME'] = str(Path(directory) / 'data')
        runtime = Path(directory) / 'runtime'
        runtime.mkdir(mode=0o700)
        env['XDG_RUNTIME_DIR'] = str(runtime)
        env['GIO_USE_VFS'] = 'local'
        env['NO_AT_BRIDGE'] = '1'
        services = Path(directory) / 'services'
        services.mkdir()
        for data_dir in env.get('XDG_DATA_DIRS', '/usr/local/share:/usr/share').split(':'):
            service = Path(data_dir) / 'dbus-1/services/org.xfce.Xfconf.service'
            if service.exists():
                shutil.copy(service, services / service.name)
                break
        config = Path(directory) / 'bus.conf'
        config.write_text(f'''<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<servicedir>{services}</servicedir><policy context="default">
<allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/>
</policy></busconfig>''')
        subprocess.run(['dbus-run-session', '--config-file', str(config), '--', sys.executable,
            __file__, directory], env=env, check=True)


if __name__ == '__main__':
    if sys.argv[1:] == ['--watcher']: serve_watcher()
    elif len(sys.argv) == 2: test(sys.argv[1])
    else: isolated()
