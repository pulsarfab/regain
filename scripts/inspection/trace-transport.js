// Research only: observe the SDK in an owned diagnostic host, never NINA.
// Passive by default. An explicit experiment may cancel ONE pending bulk read.
// Never rewrite API arguments/results. Optional bulk hashing passes bytes to the
// local Python tracer only; it records digests and discards those bytes.
'use strict';
const handles = new Set();
const pending = new Map();
let sequence = 0;
let cancelled = false;
function emit(kind, fields) { send({kind, timeMs: Date.now(), thread: Process.getCurrentThreadId(), ...fields}); }
function hex(p, length) {
    if (p.isNull() || length <= 0) return null;
    try { return Array.from(new Uint8Array(p.readByteArray(length)), b => b.toString(16).padStart(2, '0')).join(''); }
    catch (_) { return null; }
}
function u32(p) { try { return p.isNull() ? null : p.readU32(); } catch (_) { return null; } }
function controlReply(record) {
    // The observed register-read command returns at most four bytes here.
    // Do not collect general control payloads (e.g. identifiers or firmware).
    if (record.code === '0x220020' && record.header?.startsWith('c0bc')
        && record.inputLength > 38 && record.inputLength <= 42)
        return hex(record.input.add(38), record.inputLength - 38);
    return null;
}
function location(p) {
    const m = Process.findModuleByAddress(p);
    return m ? m.name + '+' + p.sub(m.base) : p.toString();
}
function attach(module, name, callbacks) {
    const address = module.findExportByName(name);
    if (address) Interceptor.attach(address, callbacks);
}
const kernel = Process.getModuleByName('kernel32.dll');
const cancelIo = new SystemFunction(kernel.getExportByName('CancelIoEx'), 'int', ['pointer', 'pointer']);
for (const name of ['CreateFileA', 'CreateFileW']) attach(kernel, name, {
    onEnter(args) {
        try {
            const path = name.endsWith('W') ? args[0].readUtf16String() : args[0].readCString();
            this.camera = /vid_03c3/i.test(path);
            this.path = path;
        } catch (_) { this.camera = false; }
    },
    onLeave(result) {
        if (this.camera && !result.equals(ptr(-1))) {
            handles.add(result.toString());
            // Path retained locally so a later read-only driver probe can select this device.
            emit('device-open', {handle: result.toString(), path: this.path});
        }
    }
});
attach(kernel, 'CloseHandle', {
    onEnter(args) { this.handle = args[0].toString(); this.camera = handles.has(this.handle); },
    onLeave(result) {
        if (this.camera && result.toInt32()) {
            handles.delete(this.handle);
            for (const [key, value] of pending) if (value.handle === this.handle) pending.delete(key);
            emit('device-close', {handle: this.handle});
        }
    }
});
attach(kernel, 'DeviceIoControl', {
    onEnter(args) {
        const handle = args[0].toString();
        if (!handles.has(handle)) return;
        this.record = {
            sequence: ++sequence, handle, code: '0x' + args[1].toUInt32().toString(16),
            inputLength: args[3].toUInt32(), outputLength: args[5].toUInt32(),
            overlapped: args[7].toString(), caller: location(this.returnAddress)
        };
        this.input = args[2]; this.output = args[4]; this.bytes = args[6];
        this.start = Date.now();
        // Only the fixed transport header, never a bulk image buffer.
        this.record.header = hex(this.input, Math.min(this.record.inputLength, 38));
        emit('io-submit', this.record);
        if (!args[7].isNull()) pending.set(handle + ':' + args[7], {
            ...this.record, input: this.input, output: this.output, start: this.start
        });
    },
    onLeave(result) {
        if (!this.record) return;
        const error = this.lastError;
        emit('io-return', {
            sequence: this.record.sequence, ok: !!result.toInt32(), lastError: error,
            bytes: result.toInt32() ? u32(this.bytes) : null, elapsedMs: Date.now() - this.start,
            header: result.toInt32() ? hex(this.input, Math.min(this.record.inputLength, 38)) : null
        });
        if (globalThis.TRACE_CANCEL_FIRST_BULK && !cancelled && this.record.code === '0x22004b'
            && !result.toInt32() && error === 997 && this.record.overlapped !== '0x0') {
            cancelled = true;
            const saved = this.record;
            setImmediate(() => {
                const value = cancelIo(ptr(saved.handle), ptr(saved.overlapped));
                emit('injected-cancel', {sequence: saved.sequence, ok: !!value.value, lastError: value.lastError});
            });
        }
        if (result.toInt32() || error !== 997) pending.delete(this.record.handle + ':' + this.record.overlapped);
    }
});
attach(kernel, 'GetOverlappedResult', {
    onEnter(args) {
        this.key = args[0] + ':' + args[1]; this.record = pending.get(this.key);
        this.bytes = args[2]; this.wait = !!args[3].toInt32();
    },
    onLeave(result) {
        if (!this.record) return;
        const error = this.lastError;
        emit('io-complete', {
            sequence: this.record.sequence, ok: !!result.toInt32(), lastError: error,
            bytes: result.toInt32() ? u32(this.bytes) : null, wait: this.wait,
            elapsedMs: Date.now() - this.record.start,
            header: hex(this.record.input, Math.min(this.record.inputLength, 38)),
            controlReply: result.toInt32() ? controlReply(this.record) : null
        });
        if (globalThis.TRACE_HASH_BULK && result.toInt32() && this.record.code === '0x22004b') {
            const length = u32(this.bytes);
            if (length > 0 && length <= this.record.outputLength && length <= 1048576)
                send({kind: 'bulk-hash-input', sequence: this.record.sequence}, this.record.output.readByteArray(length));
        }
        if (result.toInt32() || error !== 996) pending.delete(this.key);
    }
});
Process.attachModuleObserver({
    onAdded(module) {
        if (module.name.toLowerCase() !== 'asicamera2.dll') return;
        emit('sdk-module', {name: module.name});
        for (const name of ['ASIOpenCamera', 'ASIInitCamera', 'ASICloseCamera', 'ASIStartExposure',
            'ASIStopExposure', 'ASIGetExpStatus', 'ASIGetDataAfterExp']) attach(module, name, {
            onEnter(args) {
                this.start = Date.now(); this.status = name === 'ASIGetExpStatus' ? args[1] : null;
                emit('sdk-enter', {name});
            },
            onLeave(result) {
                emit('sdk-leave', {name, result: result.toInt32(), elapsedMs: Date.now() - this.start,
                    status: this.status && result.toInt32() === 0 ? u32(this.status) : null});
            }
        });
    }
});
emit('trace-ready', {});
