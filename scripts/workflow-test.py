#!/usr/bin/env python3
"""Verify the daemon operations backing the GUI/TUI workflows on loopback only."""
import importlib.util
import http.server
from pathlib import Path
import tempfile
import threading
import time

spec=importlib.util.spec_from_file_location('smoke',Path(__file__).with_name('smoke-test.py'))
smoke=importlib.util.module_from_spec(spec);spec.loader.exec_module(smoke)

class Probe(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(.5)
        self.send_response(204);self.end_headers()
    do_HEAD=do_GET
    def log_message(self,*args):pass


def main():
    with tempfile.TemporaryDirectory(prefix='oxide-workflow-') as directory:
        daemon=smoke.Daemon(directory)
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Probe)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        source=Path(directory)/'source.yaml';source.write_text(smoke.YAML)
        try:
            daemon.start();daemon.configure(source)
            state=daemon.rpc();ident=state['active_profile']
            assert state['engine']['rule_count']==1,state['engine']
            assert state['engine']['rules'][0]['target']=='Choice'
            assert state['engine']['proxies']['DIRECT']['kind']
            assert state['profiles'][0]['updated_at']>0
            daemon.rpc({'RenameProfile':{'id':ident,'name':'重命名配置'}})
            assert daemon.rpc()['profiles'][0]['name']=='重命名配置'
            assert '120' in daemon.rpc({'RenameProfile':{'id':ident,'name':'   '}},error=True)
            assert daemon.rpc()['profiles'][0]['name']=='重命名配置'
            url=f'http://127.0.0.1:{server.server_port}/generate_204'
            start=time.monotonic();daemon.rpc({'TestProxy':{'name':'Choice','url':url}})
            assert time.monotonic()-start<.4,'Latency test blocked the command actor'
            smoke.eventually(lambda:daemon.rpc()['engine']['proxies']['DIRECT']['delay'] is not None)
            smoke.eventually(lambda:daemon.rpc()['engine']['testing'] is None)
            assert daemon.rpc()['engine']['proxies']['DIRECT']['delay']>0
            with smoke.socks_connect(daemon.port,server.server_port) as first, smoke.socks_connect(daemon.port,server.server_port) as second:
                smoke.eventually(lambda:daemon.rpc()['engine']['connection_count']>=2)
                rows=daemon.rpc()['engine']['connections']
                assert all(row['source'] and row['started'] and row['rule'] for row in rows)
                daemon.rpc('CloseAllConnections')
                assert first.recv(1)==b'' and second.recv(1)==b''
            daemon.rpc({'TestProxy':{'name':'DIRECT','url':url}})
            daemon.rpc('Reload')  # Cancels a pending test and replaces the core together.
            assert daemon.rpc()['engine']['testing'] is None
            daemon.shutdown();daemon.stop();daemon.start()
            assert daemon.rpc()['profiles'][0]['name']=='重命名配置'
            print('PASS: runtime rules/details, rename validation/persistence, background latency test, close-all, test cancellation')
        finally:
            daemon.shutdown();daemon.stop();server.shutdown();server.server_close()

if __name__=='__main__':main()
