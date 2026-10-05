#!/usr/bin/env python3
"""Exercise the installer in a redirected filesystem; never install host services."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def main():
    with tempfile.TemporaryDirectory(prefix='oxide-packaging-') as temporary:
        base = Path(temporary)
        project, system, tools = (base / name for name in ['project', 'system', 'tools'])
        for path in [project / 'scripts', project / 'target/release', tools,
                     system / 'usr/share/polkit-1', system / 'etc/systemd/system', base / 'user']:
            path.mkdir(parents=True)
        shutil.copytree(ROOT / 'packaging', project / 'packaging')
        installer = (ROOT / 'scripts/install-linux.sh').read_text()
        # Run the actual shell control flow without root; redirect all installation paths.
        installer = installer.replace('-o root -g root ', '')
        for prefix in ['/etc/', '/usr/local/', '/usr/share/polkit-1', '/run/user/']:
            installer = installer.replace(prefix, str(system) + prefix)
        (project / 'scripts/install-linux.sh').write_text(installer)
        executable(tools / 'id', '#!/bin/sh\necho 0\n')
        executable(tools / 'getent', '#!/bin/sh\nprintf "%s\\n" "$OXIDE_TEST_PASSWD"\n')
        for tool in ['systemctl', 'runuser']:
            executable(tools / tool, '#!/bin/sh\nprintf "%s\\n" "$*" >> "$OXIDE_TEST_COMMANDS"\n')
        env = os.environ | {
            'PATH': str(tools) + ':' + os.environ['PATH'],
            'OXIDE_TEST_PASSWD': f'fixture-user:x:4242:4242:Fixture:{base / "user"}:/bin/sh',
            'OXIDE_TEST_COMMANDS': str(base / 'commands'),
        }
        binary = project / 'target/release/clash-oxide'
        executable(binary, '#!/bin/sh\necho gui=true\n')
        legacy_icon = system / 'usr/local/share/icons/hicolor/scalable/apps/clash-oxide.svg'
        legacy_icon.parent.mkdir(parents=True)
        legacy_icon.write_text('previous application icon')
        def install():
            subprocess.run(['bash', str(project / 'scripts/install-linux.sh'), '4242'],
                env=env, check=True, capture_output=True, text=True)
        install()
        installed = system / 'usr/local/bin/clash-oxide'
        assert installed.stat().st_mode & 0o777 == 0o755
        assert list(installed.parent.iterdir()) == [installed], 'Only the unified binary is installed'
        desktop = system / 'usr/local/share/applications/clash-oxide.desktop'
        subprocess.run(['desktop-file-validate', str(desktop)], check=True)
        assert 'Exec=clash-oxide gui' in desktop.read_text()
        icon = system / 'usr/local/share/icons/hicolor/512x512/apps/clash-oxide.png'
        assert icon.read_bytes() == (ROOT / 'packaging/clash-oxide.png').read_bytes()
        assert not legacy_icon.exists(), 'Upgrades must remove the old SVG icon'
        configuration = system / 'etc/clash-oxide/4242.env'
        assert configuration.stat().st_mode & 0o777 == 0o600
        configuration.write_text('HOME="/preserved/home"\nXDG_DATA_HOME="/preserved/data"\n')
        rule = system / 'etc/polkit-1/rules.d/49-clash-oxide-4242.rules'
        policy_test = '''
const fs = require('fs'), vm = require('vm');
let check;
vm.runInNewContext(fs.readFileSync(process.argv[1], 'utf8'), {
  polkit: {addRule: f => check = f, Result: {YES: 'yes'}}
});
function allowed(user, unit, verb, id = 'org.freedesktop.systemd1.manage-units') {
  return check({id, lookup: key => ({unit, verb})[key]}, {user}) === 'yes';
}
if (!allowed('fixture-user', 'clash-oxide@4242.service', 'start')) throw Error('owner denied');
for (const args of [
  ['another-user', 'clash-oxide@4242.service', 'start'],
  ['fixture-user', 'clash-oxide@4243.service', 'start'],
  ['fixture-user', 'sshd.service', 'start'],
  ['fixture-user', 'clash-oxide@4242.service', 'restart'],
  ['fixture-user', 'clash-oxide@4242.service', 'start', 'org.freedesktop.systemd1.manage-unit-files']
]) if (allowed(...args)) throw Error('overbroad authorization: ' + JSON.stringify(args));
'''
        subprocess.run(['node', '-e', policy_test, str(rule)], check=True)
        executable(binary, '#!/bin/sh\necho gui=false\n')
        install()
        assert not desktop.exists(), 'Headless installation must remove the GUI launcher'
        assert not icon.exists(), 'Headless installation must remove the GUI icon'
        assert configuration.read_text() == 'HOME="/preserved/home"\nXDG_DATA_HOME="/preserved/data"\n'
        unit = (system / 'etc/systemd/system/clash-oxide@.service').read_text()
        assert 'Restart=on-failure' in unit
        assert 'clash-oxide daemon --socket' in unit and 'clash-oxide daemon cleanup' in unit
        commands = (base / 'commands').read_text()
        assert 'enable' not in commands, 'Installing must not silently enable boot startup'
        print('PASS: full/headless install, desktop entry, atomic binary replacement, config preservation, Polkit owner/unit/verb boundaries (redirected filesystem)')


if __name__ == '__main__': main()
