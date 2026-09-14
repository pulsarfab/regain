"""Passive USB/SDK recording with bounded files and optional local pixel samples."""
import hashlib
import json
import re
import threading
import time

import frida

MAX_LOG = 64 * 1024 * 1024
MAX_SAMPLES = 256 * 1024 * 1024


class Trace:
    def __init__(self, host, source, directory):
        self.directory = directory
        self.log = (directory / 'events.jsonl').open('x', encoding='utf-8')
        self.lock = threading.RLock()
        self.accepting = True
        self.error = None
        self.log_bytes = self.sample_bytes = self.bulk_count = 0
        self.case = 'inventory'
        self.sample = None
        self.samples = []
        self.interfaces = set()
        self.devices = {}
        self.driver_versions = set()
        self.descriptors = []
        self.submits = {}
        self.ready = threading.Event()
        self.downloaded = threading.Event()
        self.session = self.script = None
        try:
            self.session = frida.attach(host.proc.pid)
            script = ('globalThis.TRACE_HASH_BULK = true;\n'
                      'globalThis.TRACE_CONTROL_PAYLOADS = true;\n' + source.read_text()
                      + '\nrpc.exports = {barrier() { return true; }};\n')
            self.script = self.session.create_script(script)
            self.script.on('message', self.message)
            self.script.load()
            if not self.ready.wait(10):
                raise RuntimeError('USB tracer did not initialize')
        except BaseException:
            self.close()
            raise

    def record(self, value):
        with self.lock:
            if not self.accepting or self.error:
                return
            value = dict(value)
            if value.get('kind') == 'device-open':
                # Keep the protocol/device type, not the Windows instance path.
                path = value.pop('path', '')
                match = re.search(r'vid_([0-9a-f]{4})&pid_([0-9a-f]{4})', path, re.I)
                if match:
                    value['usb'] = dict(vid=match[1].lower(), pid=match[2].lower())
                    self.interfaces.add((match[1].lower(), match[2].lower()))
                    self.devices[value['handle']] = value['usb']
            value.setdefault('timeMs', time.time_ns() // 1_000_000)
            value['exercise'] = self.case
            line = json.dumps(value) + '\n'
            size = len(line.encode())
            if self.log_bytes + size > MAX_LOG:
                self.error = 'Trace exceeded 64 MiB; bundle is incomplete'
                return
            self.log.write(line)
            self.log.flush()
            self.log_bytes += size

    def message(self, message, data):
        try:
            with self.lock:
                if not self.accepting or self.error:
                    return
                if message['type'] != 'send':
                    self.record(dict(kind='instrumentation-error', detail=message))
                    self.error = 'Instrumentation failed; inspect events.jsonl'
                    return
                value = message['payload']
                kind = value.get('kind')
                if kind == 'trace-ready':
                    self.ready.set()
                if kind == 'sdk-leave' and value.get('name') == 'ASIGetDataAfterExp':
                    self.downloaded.set()
                if kind == 'io-submit':
                    value['usb'] = self.devices.get(value.get('handle'))
                    self.submits[value['sequence']] = value
                    if value['code'] == '0x22004b':
                        self.bulk_count += 1
                if kind in ('io-complete', 'io-return') and value.get('ok'):
                    submit = self.submits.get(value['sequence'], {})
                    if submit.get('code') == '0x220000' and value.get('header'):
                        self.driver_versions.add(value['header'][:8])
                if kind == 'control-payload':
                    submit = self.submits.get(value['sequence'], {})
                    header = submit.get('header', '')
                    # Standard device/configuration descriptors; string descriptors
                    # remain only in the reviewable raw diagnostic trace.
                    if header.startswith('8006') and header[6:8] in ('01', '02'):
                        item = dict(usb=submit.get('usb'), setup=header[:16], data=value['data'])
                        if item not in self.descriptors:
                            self.descriptors.append(item)
                if kind == 'bulk-hash-input':
                    digest = hashlib.sha256(data).hexdigest()
                    event = dict(kind='bulk-data', sequence=value['sequence'], bytes=len(data), sha256=digest)
                    if self.sample:
                        if self.sample_bytes + len(data) > MAX_SAMPLES:
                            self.error = 'USB samples exceeded 256 MiB; bundle is incomplete'
                            return
                        path = self.sample / f'usb-{value["sequence"]:06d}.bin'
                        path.write_bytes(data)
                        event['file'] = path.relative_to(self.directory).as_posix()
                        self.sample_bytes += len(data)
                        self.samples.append(dict(exercise=self.case, **event))
                    self.record(event)
                else:
                    self.record(value)
        except Exception as error:
            self.error = f'Trace recording failed: {type(error).__name__}'

    def checkpoint(self):
        if self.error:
            raise RuntimeError(self.error)
        self.script.exports_sync.barrier()

    def close(self):
        if self.session:
            try:
                self.session.detach()
            except frida.InvalidOperationError:
                pass
        with self.lock:
            self.accepting = False
            self.log.close()
