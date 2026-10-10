"""Exercise private Windows Hub inputs over WSL's real IPv4 and scoped IPv6 NIC.

Requires an existing WSL distro with Perl IO::Socket::IP, IO::Select and iproute2.
Does not install packages, alter routing/firewalls, enumerate equipment, or touch
NINA. Device data is simulated; this is cross-kernel virtual-network acceptance,
not a physical LAN, real-device, TLS, or UDP discovery acceptance claim.
"""
import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / 'scripts/fixtures/hub-network-upstream.pl'
PIXELS = [[100 * x + y for y in range(3)] for x in range(4)]


def wsl(distro, *command):
    return subprocess.check_output(['wsl', '--distribution', distro, '--exec', *command],
                                   text=True, encoding='utf-8', timeout=15).strip()


def wait(read, accept=bool, seconds=15):
    deadline = time.monotonic() + seconds
    while True:
        result = read()
        if accept(result):
            return result
        if time.monotonic() >= deadline:
            raise TimeoutError('Private network fixture did not reach the expected state')
        time.sleep(0.05)


def write_routes(directory):
    catalog = [{'DeviceName': 'PRIVATE simulation ' + kind, 'DeviceType': kind,
                'DeviceNumber': 0, 'UniqueID': 'regain-wsl-' + kind} for kind in ('Camera', 'SafetyMonitor')]
    routes = {'/management/v1/configureddevices': catalog,
              '/api/v1/safetymonitor/0/interfaceversion': 2, '/api/v1/safetymonitor/0/issafe': True}
    properties = dict(interfaceversion=3, camerastate=0, exposuremin=0.001, exposuremax=10,
                      numx=4, numy=3, binx=1, biny=1, startx=0, starty=0, cameraxsize=4,
                      cameraysize=3, maxbinx=1, maxbiny=1, canasymmetricbin=False,
                      canabortexposure=True, canstopexposure=True, canpulseguide=False,
                      lastexposureduration=0.01, lastexposurestarttime='2026-10-08T00:00:00', imagearray=PIXELS)
    routes.update({f'/api/v1/camera/0/{name}': value for name, value in properties.items()})
    (directory / 'routes.tsv').write_text(''.join(key + '\t' + json.dumps(value) + '\n' for key, value in routes.items()), encoding='utf-8')
    flat = [value for column in PIXELS for value in column]
    (directory / 'image.bin').write_bytes(struct.pack('<11i', 1, 0, 0, 0, 44, 2, 8, 2, 4, 3, 0) + struct.pack('<12H', *flat))


def exercise(args, scope, evidence):
    directory = evidence / ('ipv6' if scope else 'ipv4')
    directory.mkdir()
    write_routes(directory)
    linux_directory = wsl(args.distro, 'wslpath', '-u', str(directory))
    linux_fixture = wsl(args.distro, 'wslpath', '-u', str(FIXTURE))
    upstream = host = publisher = None
    leases = []
    traffic = []
    cleanup = []
    failure = None
    upstream_pid = None
    flags = subprocess.CREATE_NO_WINDOW
    binary = (args.bin_dir / 'regain-alpaca.exe').resolve(strict=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    base = None

    def save(name, value):
        (directory / name).write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')

    def call(path, data=None, binary_response=False):
        headers = {'Accept': 'application/imagebytes'} if binary_response else {}
        if data is not None:
            headers['Content-Type'] = 'application/x-www-form-urlencoded'
        request = urllib.request.Request(base + path, None if data is None else urllib.parse.urlencode(data).encode(),
                                         headers, method='GET' if data is None else 'PUT')
        with opener.open(request, timeout=3) as response:
            body = response.read(1024 * 1024 + 1)
        assert len(body) <= 1024 * 1024
        result = body if binary_response else json.loads(body)
        traffic.append({'path': path, 'method': request.method, 'bytes': len(body),
                        'reply': None if binary_response else result})
        return result

    def device(kind, member, client=88001, data=None, binary_response=False):
        path = f'/api/v1/{kind}/40/{member}'
        reply = call(path + f'?ClientID={client}', binary_response=True) if binary_response else (
            call(path + f'?ClientID={client}') if data is None else call(path, dict(ClientID=client, **data)))
        if binary_response:
            return reply
        if reply['ErrorNumber'] in (0x402, 0x407):
            return None
        assert reply['ErrorNumber'] == 0, reply
        return reply.get('Value')

    def state():
        try:
            return json.loads((directory / 'state.json').read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return None

    def stop_upstream():
        (directory / 'stop').touch()
        if upstream is None:
            return
        try:
            upstream.wait(timeout=8)
        except subprocess.TimeoutExpired:
            # Verify the exact owned Linux command before a fallback signal;
            # never stop a distro or an unrelated process.
            if upstream_pid is None:
                raise RuntimeError('The owned Linux PID was never established')
            expected = ['perl', linux_fixture, 'ipv6' if scope else 'ipv4', args.interface, linux_directory]
            actual = wsl(args.distro, 'cat', f'/proc/{upstream_pid}/cmdline').rstrip('\0').split('\0')
            if actual != expected:
                raise RuntimeError('Owned Linux command could not be verified')
            wsl(args.distro, 'kill', '-TERM', str(upstream_pid))
            upstream.wait(timeout=8)
            raise RuntimeError('Owned upstream required fallback termination')

    with (directory / 'upstream.log').open('w') as upstream_log, (directory / 'host.log').open('w') as host_log, \
            (directory / 'publisher.log').open('w') as publisher_log:
        try:
            upstream = subprocess.Popen(['wsl', '--distribution', args.distro, '--exec', 'perl', linux_fixture,
                                         'ipv6' if scope else 'ipv4', args.interface, linux_directory],
                                        stdout=upstream_log, stderr=upstream_log, creationflags=flags)
            def ready():
                if upstream.poll() is not None:
                    raise RuntimeError('Owned upstream exited; inspect upstream.log')
                return (directory / 'ready').exists()
            wait(ready)
            ready_values = (directory / 'ready').read_text().splitlines()
            port, address = int(ready_values[0]), ready_values[1]
            upstream_pid = int(ready_values[2])
            assert 1 <= port <= 65535
            assert ipaddress.ip_address(address).is_link_local if scope else ipaddress.ip_address(address).version == 4
            save('upstream-interface.json', json.loads(wsl(args.distro, 'ip', '-j', 'address', 'show', 'dev', args.interface)))
            origin = f'http://[{address}]:{port}/' if scope else f'http://{address}:{port}/'
            sources = []
            outputs = []
            for kind in ('safetymonitor', 'camera'):
                source = str(uuid.uuid4())
                backend = dict(kind='alpaca', baseUrl=origin, deviceType=kind, deviceNumber=0,
                               uniqueId='regain-wsl-' + ('Camera' if kind == 'camera' else 'SafetyMonitor'), connectionPolicy='managed')
                if scope:
                    backend['scopeId'] = scope
                sources.append(dict(id=source, label='PRIVATE WSL simulation ' + kind, backend=backend,
                                    polling=dict(pollSeconds=0.1, requestTimeoutSeconds=2, connectionTimeoutSeconds=5)))
                definition = dict(kind='proxy', source=source, deviceType=kind) if kind == 'camera' else dict(kind='safety', members=[dict(
                    source=source, enabled=True, policy=dict(confirmationSeconds=0.1, maximumSafeAgeSeconds=3, returnToSafeHoldSeconds=0.1))])
                outputs.append(dict(id=str(uuid.uuid4()), number=40, label='PRIVATE WSL simulation ' + kind, device=definition))
            config = dict(schemaVersion=1, revision=str(uuid.uuid4()), instanceId=str(uuid.uuid4()), sources=sources, outputs=outputs)
            save('hub.json', config)
            save('profiles.json', [])
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                local_port = reservation.getsockname()[1]
            base = f'http://127.0.0.1:{local_port}'
            common = ['--hub-config', str(directory / 'hub.json'), '--workers', str(binary.parent), '--simulate']
            host = subprocess.Popen([str(binary), '--hub-host', *common], stdout=host_log, stderr=host_log, creationflags=flags)
            def host_ready():
                if host.poll() is not None:
                    raise RuntimeError('Owned host exited; inspect host.log')
                return 'Regain hub ready:' in (directory / 'host.log').read_text()
            wait(host_ready)
            publisher = subprocess.Popen([str(binary), *common, '--profiles', str(directory / 'profiles.json'), '--listen', '127.0.0.1',
                                          '--port', str(local_port), '--no-discovery'], stdout=publisher_log, stderr=publisher_log, creationflags=flags)
            def published():
                if publisher.poll() is not None:
                    raise RuntimeError('Owned publisher exited; inspect publisher.log')
                try:
                    result = call('/management/v1/configureddevices')
                    return result['ErrorNumber'] == 0 and len(result['Value']) == 2
                except urllib.error.URLError:
                    return False
            wait(published)
            for kind, client in (('safetymonitor', 88001), ('camera', 88001), ('camera', 88002)):
                leases.append((kind, client))
                device(kind, 'connected', client, {'Connected': 'true'})
                wait(lambda: device(kind, 'connected', client), lambda value: value is True)
            wait(lambda: device('safetymonitor', 'issafe'), lambda value: value is True)
            device('camera', 'startexposure', data=dict(Duration=0.01, Light='true'))
            wait(lambda: device('camera', 'imageready'), lambda value: value is True)
            assert device('camera', 'imagearray') == PIXELS
            raw = device('camera', 'imagearray', 88002, binary_response=True)
            (directory / 'published-image.bin').write_bytes(raw)
            header = struct.unpack_from('<11i', raw)
            assert header[0:2] == (1, 0) and header[7:] == (2, 4, 3, 0)
            formats = {2: 'i', 8: 'H'}
            flat = [value for column in PIXELS for value in column]
            assert len(raw) == header[4] + len(flat) * struct.calcsize(formats[header[6]])
            assert list(struct.unpack_from('<' + formats[header[6]] * len(flat), raw, header[4])) == flat
            device('camera', 'connected', data={'Connected': 'false'})
            leases.remove(('camera', 88001))
            assert device('camera', 'connected', 88002) is True
            assert device('camera', 'imagearray', 88002) == PIXELS
            before_loss = wait(state)
            assert before_loss['starts'] == 1 and before_loss['images'] == 1, before_loss
            # Stop the exact owned remote server, leaving the existing output
            # leases alive. Cached safety must expire without a frontend retry.
            stop_upstream()
            assert upstream.returncode == 0
            began = time.monotonic()
            wait(lambda: device('safetymonitor', 'issafe'), lambda value: value is False, seconds=4)
            remote_traffic = [line.split('\t') for line in (directory / 'upstream.tsv').read_text().splitlines()]
            assert remote_traffic
            peers = {item[0] for item in remote_traffic}
            assert all(not ipaddress.ip_address(peer.split('%')[0]).is_loopback for peer in peers), peers
            assert all(ipaddress.ip_address(peer.split('%')[0]).version == (6 if scope else 4) for peer in peers), peers
            assert any(item[2] == '/management/v1/configureddevices' for item in remote_traffic)
            assert any(item[2] == '/api/v1/camera/0/imagearray' and item[4] == 'application/imagebytes' for item in remote_traffic)
            save('result.json', dict(simulationOnly=True, origin=origin, scopeId=scope, pixels=len(flat),
                                    remotePeers=sorted(peers),
                                    exactAcrossFormatsAndClients=True, starts=before_loss['starts'], upstreamImageReads=before_loss['images'],
                                    safetyWithdrawnSeconds=time.monotonic() - began))
        except BaseException as error:
            failure = repr(error)
        finally:
            if upstream is not None and upstream.poll() is None:
                for kind, client in reversed(leases):
                    try:
                        device(kind, 'connected', client, {'Connected': 'false'})
                        cleanup.append(dict(kind=kind, client=client, released=True))
                    except Exception as error:
                        cleanup.append(dict(kind=kind, client=client, released=False, error=repr(error)))
            for process in (publisher, host):
                if process is not None:
                    try:
                        if process.poll() is None:
                            process.terminate()
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        try:
                            process.kill()
                            process.wait(timeout=5)
                        except Exception as error:
                            cleanup.append(dict(processStopped=False, error=repr(error)))
                    except Exception as error:
                        cleanup.append(dict(processStopped=False, error=repr(error)))
            try:
                stop_upstream()
            except Exception as error:
                cleanup.append(dict(upstreamStopped=False, error=repr(error)))
            stopped = all(process is None or process.poll() is not None for process in (upstream, publisher, host))
            save('traffic.json', traffic)
            save('summary.json', dict(failure=failure, cleanup=cleanup, ownedProcessesStopped=stopped,
                                     upstreamPid=upstream_pid, upstreamExitCode=None if upstream is None else upstream.poll()))
    assert stopped and failure is None and all(item.get('released', True) and item.get('upstreamStopped', True)
                                               and item.get('processStopped', True) for item in cleanup), directory
    return json.loads((directory / 'result.json').read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--distro', default='Debian')
    parser.add_argument('--interface', default='eth0')
    parser.add_argument('--windows-scope', required=True, type=int)
    parser.add_argument('--bin-dir', type=Path, default=ROOT / 'target/debug')
    args = parser.parse_args()
    if not __debug__ or os.name != 'nt' or not 1 <= args.windows_scope <= 4294967295:
        parser.error('Requires Windows, a positive interface scope and enabled Python assertions')
    evidence = ROOT / 'artifacts' / ('hub-wsl-network-' + uuid.uuid4().hex)
    evidence.mkdir()
    print(f'Cross-kernel virtual-network evidence: {evidence}', flush=True)
    binary = (args.bin_dir / 'regain-alpaca.exe').resolve(strict=True)
    provenance = dict(distro=args.distro, kernel=wsl(args.distro, 'uname', '-sr'), interface=args.interface,
                      serverSha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                      testSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      windowsScope=args.windows_scope, fixtureSha256=hashlib.sha256(FIXTURE.read_bytes()).hexdigest())
    (evidence / 'provenance.json').write_text(json.dumps(provenance, indent=2))
    results = [exercise(args, None, evidence), exercise(args, args.windows_scope, evidence)]
    (evidence / 'summary.json').write_text(json.dumps(dict(results=results), indent=2))
    print('IPv4 and scoped IPv6: exact shared images and safety expiry passed; owned processes stopped', flush=True)


if __name__ == '__main__':
    main()
