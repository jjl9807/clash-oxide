#!/usr/bin/env python3
"""Real native GUI/TUI interaction checks against the Party-style workflows."""
import importlib.util
import fcntl
import struct
import termios
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def load(name):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(name+'.py'))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

smoke=load('smoke-test'); lifecycle=load('lifecycle-test'); ui=load('ui-smoke-test')
OUT=smoke.ROOT/'target/ui-party'


def tui_test(daemon,source):
    app=lifecycle.Tui(daemon)
    def send(data): os.write(app.master,data);time.sleep(.2)
    try:
        smoke.eventually(lambda:'Daemon connected' in smoke.terminal_title(app.output))
        send(b'\x1bOQ')  # F2 profiles
        send(b'n')
        send('终端配置'.encode()+b'\t')
        send(b'\x1b[200~'+str(source).encode()+b'\x1b[201~\r')
        smoke.eventually(lambda:any(p['name']=='终端配置' for p in daemon.rpc()['profiles']))
        smoke.eventually(lambda:daemon.rpc()['phase']=='Running')
        send(b'\x1bOP')  # F1 groups
        send(b'/Choice\r')
        send(b'\x1b')
        send(b'\x1bOS')  # F4 rules
        send(b'/MATCH\r')
        time.sleep(.3)
        send(b'\x1b[15~')  # F5 logs
        send(b' ');send(b' ')
        send(b'?');send(b'\x1b')
        send(b'\x1b[17~')  # F6 settings
        send(b'\x1b[B'*4+b'\r') # Chinese
        preference=Path(os.environ['XDG_CONFIG_HOME'])/'clash-oxide/frontend.json'
        smoke.eventually(lambda:preference.exists() and json.loads(preference.read_text())['language']=='zh-CN')
        send(b'\x1b[A\r') # English
        smoke.eventually(lambda:json.loads(preference.read_text())['language']=='en')
        send(b'\x1bOQn');send(b'Temporary\t');send(str(source).encode()+b'\r')
        smoke.eventually(lambda:len(daemon.rpc()['profiles'])==2)
        send(b'\x1b[B'*2+b'd');send(b'\r')  # Default action is Cancel.
        assert len(daemon.rpc()['profiles'])==2
        send(b'd\t\r')
        smoke.eventually(lambda:len(daemon.rpc()['profiles'])==1)
        fcntl.ioctl(app.master,termios.TIOCSWINSZ,struct.pack('HHHH',20,60,0,0))
        send(b'\x1bOP');send(b'\t\r')  # Narrow layout: Body -> Toolbar -> search.
        send(b'Choice\r');send(b'\x1b')
        send(b'\x1b[<0;55;3M\x1b[<0;55;3m')  # Click F6 in the narrow navigation.
        send(b'\x1b[B'*4+b'\r')
        smoke.eventually(lambda:json.loads(preference.read_text())['language']=='zh-CN')
        send(b'\x1b[A\r')
        smoke.eventually(lambda:json.loads(preference.read_text())['language']=='en')
        fcntl.ioctl(app.master,termios.TIOCSWINSZ,struct.pack('HHHH',12,40,0,0))
        time.sleep(.3)  # Minimum-size warning, then restore without losing the session.
        fcntl.ioctl(app.master,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0))
        send(b'\x1bOQ')
        print('PASS: TUI import, Unicode/paste, auto-start, page/search/help/language, delete confirmation, narrow layout and resize',flush=True)
    finally:
        (OUT/'tui.ansi').write_bytes(app.output);app.close()


def gui_test(daemon,source,capture_only=False):
    env=os.environ.copy();env.pop('WAYLAND_DISPLAY',None)
    env['DBUS_SESSION_BUS_ADDRESS']='unix:path='+str(daemon.directory/'no-session-bus')
    xvfb=subprocess.Popen(['Xvfb','-displayfd','1','-screen','0','1440x1000x24','-nolisten','tcp'],env=env,stdout=subprocess.PIPE,stderr=open(OUT/'xvfb.log','w'),text=True)
    app=None
    try:
        number=xvfb.stdout.readline().strip();assert number.isdigit();env['DISPLAY']=':'+number
        app=subprocess.Popen([str(smoke.BINARY),'gui','--socket',str(daemon.sock),'--data-dir',str(daemon.data)],env=env,stdout=open(OUT/'gui.log','w'),stderr=subprocess.STDOUT)
        def xdo(*args):return subprocess.check_output(['xdotool',*args],env=env,text=True,stderr=subprocess.DEVNULL)
        def window_id():
            try:return xdo('search','--onlyvisible','--name','^Clash Oxide$').splitlines()[0]
            except subprocess.CalledProcessError:return None
        window=smoke.eventually(window_id);time.sleep(1)
        xdo('windowfocus','--sync',window)
        def key(*args):xdo('key','--clearmodifiers',*args);time.sleep(.3)
        def click(x,y):xdo('mousemove','--window',window,str(x),str(y),'click','1');time.sleep(.3)
        def shot(name):time.sleep(.4);subprocess.run(['import','-display',env['DISPLAY'],'-window',window,str(OUT/(name+'.png'))],env=env,check=True)
        for page,name in enumerate(['proxies','profiles','connections','rules','logs','settings'],1):
            key('ctrl+'+str(page));shot(name)
        if capture_only:return
        key('ctrl+2')
        # Coordinates of the import toolbar are validated by the captured native frames.
        click(360,148);xdo('type','--clearmodifiers','GUI profile')
        click(650,148);xdo('type','--clearmodifiers',str(source));key('Return')
        smoke.eventually(lambda:any(p['name']=='GUI profile' for p in daemon.rpc()['profiles']))
        shot('profile-imported')
        click(795,310)  # Rename the imported card
        shot('rename-dialog')
        if '--capture-dialog' in sys.argv:return
        click(500,137);key('ctrl+a');xdo('type','--clearmodifiers','Renamed GUI');key('Tab');key('Tab');key('Return')
        smoke.eventually(lambda:any(p['name']=='Renamed GUI' for p in daemon.rpc()['profiles']))
        # Cancellation must preserve the profile; confirmation must preserve its source file.
        click(865,310);shot('delete-dialog');key('Escape')
        assert any(p['name']=='Renamed GUI' for p in daemon.rpc()['profiles'])
        click(865,310);key('Tab');key('Tab');key('Return')
        smoke.eventually(lambda:not any(p['name']=='Renamed GUI' for p in daemon.rpc()['profiles']))
        assert source.exists()
        key('Escape')  # Leave the page ready for keyboard navigation
        key('ctrl+1');click(385,140);shot('proxy-grid')
        click(875,198)
        smoke.eventually(lambda:next(g for g in daemon.rpc()['engine']['groups'] if g['name']=='Choice')['selected']=='REJECT')
        key('Escape');click(470,198)
        smoke.eventually(lambda:next(g for g in daemon.rpc()['engine']['groups'] if g['name']=='Choice')['selected']=='DIRECT')
        # A failed import must leave both fields intact so the source can be corrected.
        key('ctrl+2');click(360,148);xdo('type','--clearmodifiers','Retained draft')
        click(650,148);xdo('type','--clearmodifiers',str(source)+'.missing');key('Return')
        smoke.eventually(lambda:daemon.rpc()['last_error'])
        shot('import-error')
        click(650,148);key('ctrl+a');xdo('type','--clearmodifiers',str(source));key('Return')
        smoke.eventually(lambda:any(p['name']=='Retained draft' for p in daemon.rpc()['profiles']))
        # View preferences are independent of the saved language.
        key('ctrl+6');click(600,531);key('ctrl+a');xdo('type','--clearmodifiers','http://127.0.0.1:12345/probe');click(1040,531)
        preference=Path(os.environ['XDG_CONFIG_HOME'])/'clash-oxide/view.json'
        smoke.eventually(lambda:json.loads(preference.read_text())['test_url']=='http://127.0.0.1:12345/probe')
        assert json.loads(preference.with_name('frontend.json').read_text())['language']=='en'
        with socket.socket() as listener:
            listener.bind(('127.0.0.1',0));listener.listen(4)
            with smoke.socks_connect(daemon.port,listener.getsockname()[1]) as first, smoke.socks_connect(daemon.port,listener.getsockname()[1]) as second:
                smoke.eventually(lambda:daemon.rpc()['engine']['connection_count']>=2)
                key('ctrl+3');time.sleep(1.1);shot('connections-live')
                click(500,194);shot('connection-detail');key('Escape')
                click(796,84);shot('connections-paused');click(796,84)
                click(1000,84);shot('close-all-dialog');key('Escape')
                assert daemon.rpc()['engine']['connection_count']>=2
                click(1000,84);key('Tab');key('Tab');key('Return')
                smoke.eventually(lambda:daemon.rpc()['engine']['connection_count']==0)
                assert first.recv(1)==b'' and second.recv(1)==b''
                time.sleep(1.1);click(730,84);shot('connections-closed')
        print('PASS: GUI pages, imports/error recovery, rename/delete confirmation, proxy selection, saved preferences, connection details/pause/close-all',flush=True)
        ui.close_window(env['DISPLAY'],window);assert app.wait(timeout=10)==0
        assert daemon.rpc()['phase']=='Running'
    except Exception:
        if app and app.poll() is None:
            shot('failure')
            (OUT/'failure-state.json').write_text(json.dumps(daemon.rpc(),ensure_ascii=False,indent=2))
        raise
    finally:
        if app and app.poll() is None:app.terminate();app.wait(timeout=10)
        xvfb.terminate();xvfb.wait(timeout=5)


def main():
    OUT.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='oxide-party-') as directory:
        os.environ['CLASH_OXIDE_LANG']='en';os.environ['XDG_CONFIG_HOME']=str(Path(directory)/'config')
        daemon=smoke.Daemon(directory);source=Path(directory)/'profile.yaml';source.write_text(smoke.YAML)
        try:
            daemon.start();daemon.rpc({'SetMixedPort':daemon.port})
            if '--capture-only' not in sys.argv:tui_test(daemon,source)
            else:daemon.configure(source)
            if '--tui-only' not in sys.argv:gui_test(daemon,source,'--capture-only' in sys.argv)
        finally:daemon.shutdown();daemon.stop()

if __name__=='__main__':main()
