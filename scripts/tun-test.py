#!/usr/bin/env python3
"""Run ONLY inside a fresh namespace: unshare -Urn python3 scripts/tun-test.py.

A second namespace hosts test HTTP, UDP and DNS servers. No internet is used.
"""
import http.server
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('smoke-test.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)

V4 = '198.51.100.2'
V6 = 'fd00:2027::2'

def ip(*args):
    command = ['ip', *map(str, args)]
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise RuntimeError(f'{command}: {result.stdout.strip()}')
    return result.stdout

def udp_server(address, port, dns=False):
    family = socket.AF_INET6 if ':' in address else socket.AF_INET
    with socket.socket(family, socket.SOCK_DGRAM) as sock:
        sock.bind((address, port))
        while True:
            data, peer = sock.recvfrom(65535)
            if dns:
                end = 12
                while data[end]: end += data[end] + 1
                end += 1
                kind = struct.unpack('!H', data[end:end + 2])[0]
                value = socket.inet_pton(socket.AF_INET6 if kind == 28 else socket.AF_INET, V6 if kind == 28 else V4)
                data = data[:2] + b'\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00' + data[12:end+4] + b'\xc0\x0c' + struct.pack('!HHIH', kind, 1, 30, len(value)) + value
            sock.sendto(data, peer)

def server(directory):
    smoke.eventually(lambda: (directory / 'network-ready').exists())
    class IPv6Server(http.server.ThreadingHTTPServer): address_family = socket.AF_INET6
    for family, address in [(http.server.ThreadingHTTPServer, V4), (IPv6Server, V6)]:
        http_service = family((address, 8080), smoke.Handler)
        threading.Thread(target=http_service.serve_forever, daemon=True).start()
        threading.Thread(target=udp_server, args=(address, 9000), daemon=True).start()
    threading.Thread(target=udp_server, args=('192.0.2.2', 53, True), daemon=True).start()
    (directory / 'server-ready').touch()
    while True: time.sleep(30)

def get(address):
    with socket.create_connection((address, 8080), timeout=5) as sock:
        sock.sendall(b'GET /ok HTTP/1.0\r\nHost: demo.test\r\n\r\n')
        data = b''
        while chunk := sock.recv(8192): data += chunk
        assert b'oxide proxy works' in data, data

def udp(address):
    with socket.socket(socket.AF_INET6 if ':' in address else socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(5)
        sock.sendto(b'oxide datagram', (address, 9000))
        assert sock.recv(4096) == b'oxide datagram'

def dns(fake=False):
    # Arbitrary external DNS address: TUN must hijack it to the configured resolver.
    query = b'\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x04demo\x04test\x00\x00\x01\x00\x01'
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(5); sock.sendto(query, ('8.8.8.8', 53))
        response = sock.recv(4096)
        assert response[:2] == b'\x12\x34' and response[3] & 15 == 0, response
        if not fake:
            assert socket.inet_aton(V4) in response, response
        else:
            offset = 12
            while response[offset]: offset += response[offset] + 1
            offset += 5  # root label + question type/class
            assert response[offset] & 0xc0 == 0xc0, response
            kind, _, _, length = struct.unpack('!HHIH', response[offset + 2:offset + 12])
            assert kind == 1 and length == 4
            address = socket.inet_ntoa(response[offset + 12:offset + 16])
            assert ipaddress.ip_address(address) in ipaddress.ip_network('198.18.0.0/16')
            return address

def main():
    # Refuse to operate in the host namespace or an existing configured namespace.
    assert os.getuid() == 0, 'Use unshare -Urn'
    # An unprivileged user namespace maps just the calling uid to namespace root.
    assert Path('/proc/self/uid_map').read_text().split()[-1] == '1', 'Requires an unprivileged user namespace'
    assert {x['ifname'] for x in json.loads(ip('-j', 'link'))} == {'lo'}, 'Requires a fresh network namespace'
    with tempfile.TemporaryDirectory(prefix='oxide-tun-') as directory:
        directory = Path(directory)
        peer = subprocess.Popen(['unshare', '-n', sys.executable, __file__, '--server', str(directory)])
        daemon = smoke.Daemon(directory)
        try:
            smoke.eventually(lambda: os.readlink(f'/proc/{peer.pid}/ns/net') != os.readlink('/proc/self/ns/net'))
            ip('link', 'set', 'lo', 'up')
            ip('link', 'add', 'eth0', 'type', 'veth', 'peer', 'name', 'peer0')
            ip('link', 'set', 'peer0', 'netns', peer.pid)
            ip('addr', 'add', '192.0.2.1/24', 'dev', 'eth0')
            ip('-6', 'addr', 'add', 'fd00:2026::1/64', 'dev', 'eth0', 'nodad')
            ip('link', 'set', 'eth0', 'up')
            def remote(*args):
                subprocess.run(['nsenter', '-t', str(peer.pid), '-n', 'ip', *args], check=True)
            remote('link', 'set', 'lo', 'up')
            remote('addr', 'add', '192.0.2.2/24', 'dev', 'peer0')
            remote('-6', 'addr', 'add', 'fd00:2026::2/64', 'dev', 'peer0', 'nodad')
            remote('link', 'set', 'peer0', 'up')
            remote('addr', 'add', V4 + '/32', 'dev', 'lo')
            remote('-6', 'addr', 'add', V6 + '/128', 'dev', 'lo', 'nodad')
            ip('route', 'add', 'default', 'via', '192.0.2.2')
            ip('-6', 'route', 'add', 'default', 'via', 'fd00:2026::2')
            ip('rule', 'add', 'pref', '21000', 'to', '203.0.113.0/24', 'lookup', 'main')
            baseline = {f: json.loads(ip(f, '-j', 'rule')) for f in ['-4', '-6']}
            (directory / 'network-ready').touch()
            smoke.eventually(lambda: (directory / 'server-ready').exists())
            get(V4); get(V6)
            config = directory / 'tun.yaml'
            config.write_text(smoke.YAML + 'dns:\n  ipv6: true\n  nameserver: [192.0.2.2]\n')
            daemon.start(); profile_id = daemon.configure(config)
            daemon.rpc({'SetTun': True})
            assert daemon.rpc()['tun_active']
            assert 'oxide0' in ip('route', 'get', V4)
            assert 'oxide0' in ip('-6', 'route', 'get', V6)
            get(V4); get(V6); udp(V4); udp(V6); dns()
            def transferred():
                state = daemon.rpc()
                return state if state['engine']['traffic']['download_total'] else None
            state = smoke.eventually(transferred)
            assert state['engine']['traffic']['download_total'] > 0
            # URL imports must also work while TUN captures daemon egress.
            daemon.rpc({'Import': {'name': 'Through TUN', 'source': f'http://{V4}:8080/profile'}})
            daemon.rpc('Reload'); get(V4); dns()
            daemon.stop(kill=True)
            assert (daemon.data / 'routes.json').exists()
            daemon.start()
            smoke.eventually(lambda: daemon.rpc()['tun_active'])
            get(V4); get(V6)
            config.write_text(smoke.YAML + 'dns:\n  ipv6: true\n  enhanced-mode: fake-ip\n  fake-ip-range: 198.18.0.1/16\n  nameserver: [192.0.2.2]\n')
            daemon.rpc({'Refresh': {'id': profile_id}})
            fake_address = dns(fake=True)
            get(fake_address)
            # Back-to-back replacements must release the old TUN device even
            # with the upstream no-op TunRunner::join implementation.
            for _ in range(3):
                daemon.rpc('Reload')
                get(fake_address)  # Clients may retain the DNS answer across reload.
            daemon.shutdown()
            assert not Path('/sys/class/net/oxide0').exists()
            assert not (daemon.data / 'routes.json').exists()
            for family in ['-4', '-6']:
                assert json.loads(ip(family, '-j', 'rule')) == baseline[family]
            daemon.start()
            smoke.eventually(lambda: daemon.rpc()['tun_active'])
            get(fake_address)
            daemon.rpc({'SetTun': False})
            assert 'oxide0' not in ip('route', 'get', V4)
            assert b'oxide proxy works' in smoke.http_get(daemon.port, 8080, host=V4)
            for family in ['-4', '-6']:
                assert json.loads(ip(family, '-j', 'rule')) == baseline[family]
            assert not (daemon.data / 'routes.json').exists()
            assert not Path('/sys/class/net/oxide0').exists()
            # Ordinary proxy mode must not retain the previous TUN interface
            # binding in DNS clients when the network changes.
            ip('link', 'set', 'eth0', 'down')
            ip('link', 'set', 'eth0', 'name', 'uplink0')
            ip('link', 'set', 'uplink0', 'up')
            # Linux can remove addresses/routes on link-down. Restore the test
            # network before testing the daemon's previous interface binding.
            ip('addr', 'replace', '192.0.2.1/24', 'dev', 'uplink0')
            ip('-6', 'addr', 'replace', 'fd00:2026::1/64', 'dev', 'uplink0', 'nodad')
            ip('route', 'replace', 'default', 'via', '192.0.2.2', 'dev', 'uplink0')
            ip('-6', 'route', 'replace', 'default', 'via', 'fd00:2026::2', 'dev', 'uplink0')
            get(V4); get(V6)
            assert b'oxide proxy works' in smoke.http_get(daemon.port, 8080, host='after-tun.test')
            daemon.shutdown()
            get(V4); get(V6)
            print('PASS: TUN IPv4/IPv6 TCP + UDP, DNS hijack + fake-IP, subscription import, repeated reload, crash recovery, disable/cleanup, DNS after interface change, unrelated policy retained')
        except Exception:
            print(daemon.logs()[-18000:]); raise
        finally:
            daemon.stop(); peer.terminate(); peer.wait(timeout=5)

if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--server': server(Path(sys.argv[2]))
    else: main()
