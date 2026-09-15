// Research only: observe the SDK in an owned diagnostic host, never NINA.
// Passive by default. An explicit experiment may cancel ONE pending bulk read.
// Never rewrite API arguments/results. Optional bulk hashing passes bytes to the
// local Python tracer only; it records digests and discards those bytes.
'use strict';
const handles = new Set();
const pending = new Map();
let sequence = 0;
let cancelled = false;
let bulkNumber = 0;
function emit(kind, fields) { send({kind, timeMs: Date.now(), thread: Process.getCurrentThreadId(), ...fields}); }
function hex(p, length) {
    if (p.isNull() || length <= 0) return null;
    try { return Array.from(new Uint8Array(p.readByteArray(length)), b => b.toString(16).padStart(2, '0')).join(''); }
    catch (_) { return null; }
}
function u32(p) { try { return p.isNull() ? null : p.readU32(); } catch (_) { return null; } }
function collectBulk(record, input, output, length) {
    // The camera exercise kit needs model-independent control responses and
    // calibration, without calling version-specific SDK internal functions.
    if (globalThis.TRACE_CONTROL_PAYLOADS && record.code === '0x220020' && record.inputLength >= 38) {
        const offset = u32(input.add(30)), size = u32(input.add(34));
        if (offset === 38 && size > 0 && size <= 65536 && length >= offset + size
            && record.inputLength >= offset + size)
            emit('control-payload', {sequence: record.sequence, bytes: size,
                data: hex(input.add(offset), size)});
    }
    if (globalThis.TRACE_PROCESSING && record.code === '0x220020' && record.header?.startsWith('c0c3')
        && length > 38 && length <= 2086) {
        const index = input.add(4).readU16();
        send({kind:'calibration-buffer',name:'eeprom-'+index},input.add(38).readByteArray(length-38));
    }
    if (!globalThis.TRACE_HASH_BULK || record.code !== '0x22004b') return;
    if (u32(input.add(14)) === 0 && u32(input.add(18)) === 0
        && length > 0 && length <= record.outputLength && length <= 1048576)
        send({kind: 'bulk-hash-input', sequence: record.sequence}, output.readByteArray(length));
    else emit('bulk-payload-skipped', {sequence: record.sequence, bytes: length});
}
function controlReply(record) {
    // The observed register-read command returns at most four bytes here.
    // Do not collect general control payloads (e.g. identifiers or firmware).
    if (record.code === '0x220020' && (record.header?.startsWith('c0bc') || record.header?.startsWith('c0b3'))
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
        if (this.record.code === '0x22004b') this.record.bulkNumber = ++bulkNumber;
        this.input = args[2]; this.output = args[4]; this.bytes = args[6];
        this.start = Date.now();
        // Only the fixed transport header, never a bulk image buffer.
        this.record.header = hex(this.input, Math.min(this.record.inputLength, 38));
        emit('io-submit', this.record);
        if (globalThis.TRACE_CONTROL_PAYLOADS && this.record.code === '0x220020'
            && (this.input.readU8() & 0x80) === 0 && this.record.inputLength > 38
            && this.record.inputLength <= 38 + 65536)
            emit('control-output', {sequence: this.record.sequence,
                data: hex(this.input.add(38), this.record.inputLength - 38)});
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
        if (result.toInt32()) collectBulk(this.record, this.input, this.output, u32(this.bytes));
        if (Number.isInteger(globalThis.TRACE_CANCEL_BULK_NUMBER) && this.record.code === '0x22004b'
            && globalThis.TRACE_CANCEL_BULK_NUMBER === this.record.bulkNumber && !cancelled
            && !result.toInt32() && error === 997 && this.record.overlapped !== '0x0') {
            // Cancel on this submission's thread while its OVERLAPPED is still
            // alive. A later callback could race reuse of the same stack address.
            cancelled = true;
            const value = cancelIo(ptr(this.record.handle), ptr(this.record.overlapped));
            emit('injected-cancel', {sequence: this.record.sequence, bulkNumber: this.record.bulkNumber,
                ok: !!value.value, lastError: value.lastError});
            this.lastError = error;
        }
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
        if (result.toInt32()) collectBulk(this.record, this.record.input, this.record.output, u32(this.bytes));
        if (result.toInt32() || error !== 996) pending.delete(this.key);
    }
});
Process.attachModuleObserver({
    onAdded(module) {
        if (module.name.toLowerCase() !== 'asicamera2.dll') return;
        emit('sdk-module', {name: module.name});
        if (globalThis.TRACE_PROCESSING) {
            Interceptor.attach(module.base.add(0x10ec60), {onEnter(args) {
                emit('cooler-calibration', {type:args[0].add(0x260).readU32(),
                    values:[0x868,0x86c,0x870,0x874].map(o=>args[0].add(o).readFloat())});
            }});
            Interceptor.attach(module.base.add(0x10bbf0), {
                onEnter(args) {
                    if (args[1].toUInt32() === 0x1ee)
                        emit('duo-sensor-gate', {value:args[2].toUInt32() & 255,
                            stack:Thread.backtrace(this.context, Backtracer.ACCURATE).map(location).slice(0,8)});
                }
            });
            Interceptor.attach(module.base.add(0xd26a3), {
                onEnter() { emit('guide-dither-seed', {seed:this.context.rcx.toUInt32()}); }
            });
            // Duo call sites verified against the same hash-pinned SDK as the
            // ASI676 hooks. Keep raw data in the bounded in-memory comparator.
            for (const [entry, retrieved, before, after, model, buffer, length] of [
                [0x14f130, 0x14f1ed, 0x14f2fa, 0x14f302, 'asi2600mm-duo', 'rsi', 'r13'],
                [0xd2590, 0xd261c, 0xd2767, 0xd276f, 'asi220mm-mini', 'rbp', 'r12'],
                [0x1f02c0, 0x1f0382, 0x1f0487, 0x1f048f, 'asi6200mm-pro', 'rsi', 'r14']
            ]) {
                const calls = new Map();
                Interceptor.attach(module.base.add(entry), {
                    onEnter(args) { calls.set(this.threadId, {camera:args[0], requested:args[2].toUInt32()}); },
                    onLeave() { calls.delete(this.threadId); }
                });
                for (const [rva, stage] of [[retrieved, 'retrieved'], [before, 'before-correction'], [after, 'after-correction']]) {
                    Interceptor.attach(module.base.add(rva), {
                        onEnter() {
                            const call = calls.get(this.threadId);
                            if (!call) return;
                            if (stage === 'retrieved') {
                                if ((this.context.rax.toUInt32() & 255) === 0) return;
                                call.buffer = this.context[buffer];
                                call.length = this.context[length].toUInt32();
                            }
                            if (!call.buffer || !(call.length > 0 && call.length <= 128 * 1024 * 1024)) return;
                            if (stage === 'before-correction') {
                                const camera = call.camera;
                                const fields = {model, enabled:camera.add(0x109).readU8(),step:camera.add(0x2d4).readU8()+1,
                                    depth:camera.add(0x2b0).readU8(),width:camera.add(0x540).readU32(),height:camera.add(0x544).readU32(),
                                    hpcCount:camera.add(0x440).readU32(),deadCount:camera.add(0x55c).readU32(),
                                    rowMap:!camera.add(0x500).readPointer().isNull(),columnMap:!camera.add(0x508).readPointer().isNull(),
                                    wireBytes:call.length, requestedBytes:call.requested, bin:camera.add(0x90).readU32(),
                                    hardwareBin:camera.add(0xa7).readU8()};
                                emit('correction-layout', fields);
                                for (const [name,offset,count] of [['hpc',0x448,fields.hpcCount],['dead',0x560,fields.deadCount]]) {
                                    const table = camera.add(offset).readPointer();
                                    if (count > 0 && count <= 100000 && !table.isNull())
                                        send({kind:'calibration-buffer',name},table.readByteArray(count*4));
                                }
                            }
                            send({kind:'processing-buffer',stage},call.buffer.readByteArray(call.length));
                        }
                    });
                }
            }
            Interceptor.attach(module.base.add(0x1ef7c7), {
                onEnter() {
                    const camera = this.context.rdi;
                    emit('asi6200-exposure-timing', {height:camera.add(0x80).readU32(), bin:camera.add(0x90).readU32(),
                        clock:camera.add(0xb8).readU32(), hmax:camera.add(0xc0).readU16(),
                        minimumUs:camera.add(0xc4).readU32(), blanking:module.base.add(0x278be8).readU32(),
                        baseHmax:module.base.add(0x27975c).readU32()});
                }
            });
            Interceptor.attach(module.base.add(0x14e5ee), {
                onEnter() {
                    const camera = this.context.rdi;
                    emit('duo-exposure-timing', {height:camera.add(0x80).readU32(), bin:camera.add(0x90).readU32(),
                        clock:camera.add(0xb8).readU32(), hmax:camera.add(0xc0).readU16(),
                        minimumUs:camera.add(0xc4).readU32(), blanking:module.base.add(0x284574).readU32()});
                }
            });
            Interceptor.attach(module.base.add(0x58284), {
                onEnter() {
                    const camera = this.context.rdi;
                    emit('exposure-timing', {height:camera.add(0x80).readU32(),
                        clock:camera.add(0xb8).readU32(), hmax:camera.add(0xc0).readU16(),
                        minimumUs:camera.add(0xc4).readU32(), blanking:module.base.add(0x2955fc).readU32()});
                }
            });
            Interceptor.attach(module.base.add(0x4126), {
                onEnter() {
                    emit('control-vtable', {entries:Array.from({length:33},(_,i)=>location(this.context.rax.add(i*8).readPointer()))});
                    emit('control-target', {target: location(this.context.rax.add(0xa8).readPointer()),
                        gain: location(this.context.rax.add(0x28).readPointer()),
                        exposure: location(this.context.rax.add(0x88).readPointer())});
                }
            });
            // Version-pinned call sites, established by disassembly of SDK 1.41 x64.
            Interceptor.attach(module.base.add(0x52b6), {
                onEnter() { emit('retrieval-target', {target: location(this.context.rax.add(0x90).readPointer())}); }
            });
            for (const [rva, stage] of [[0x16cbb, 'retrieved'], [0x16dbc, 'before-correction'], [0x16dc4, 'after-correction']]) {
                Interceptor.attach(module.base.add(rva), {
                    onEnter() {
                        if (stage === 'retrieved' && (this.context.rax.toUInt32() & 255) === 0) return;
                        const length = this.context.r12.toUInt32();
                        if (stage === 'before-correction') {
                            const camera = this.context.rbx;
                            const fields = {enabled:camera.add(0x109).readU8(),step:camera.add(0x2d4).readU8()+1,
                                depth:camera.add(0x2b0).readU8(),width:camera.add(0x540).readU32(),height:camera.add(0x544).readU32(),
                                hpcCount:camera.add(0x440).readU32(),deadCount:camera.add(0x55c).readU32(),
                                rowMap:!camera.add(0x500).readPointer().isNull(),columnMap:!camera.add(0x508).readPointer().isNull()};
                            emit('correction-layout',fields);
                            if (fields.hpcCount <= 100000 && fields.deadCount <= 100000) {
                                for (const [name,offset,count] of [['hpc',0x448,fields.hpcCount],['dead',0x560,fields.deadCount]]) {
                                    const table = camera.add(offset).readPointer();
                                    if (count > 0 && !table.isNull()) send({kind:'calibration-buffer',name},table.readByteArray(count*4));
                                }
                            }
                        }
                        if (length > 0 && length <= 32 * 1024 * 1024)
                            send({kind: 'processing-buffer', stage}, this.context.rsi.readByteArray(length));
                    }
                });
            }
        }
        for (const name of ['ASIOpenCamera', 'ASIInitCamera', 'ASICloseCamera', 'ASIStartExposure', 'ASISetControlValue', 'ASISetROIFormat', 'ASISetStartPos',
            'ASIStopExposure', 'ASIGetExpStatus', 'ASIGetDataAfterExp']) attach(module, name, {
            onEnter(args) {
                this.start = Date.now(); this.status = name === 'ASIGetExpStatus' ? args[1] : null;
                const parameters = name === 'ASISetControlValue' ? [args[1].toInt32(), args[2].toInt32(), args[3].toInt32()]
                    : name === 'ASISetROIFormat' ? [args[1].toInt32(), args[2].toInt32(), args[3].toInt32(), args[4].toInt32()]
                    : name === 'ASISetStartPos' ? [args[1].toInt32(), args[2].toInt32()] : undefined;
                emit('sdk-enter', {name, parameters});
            },
            onLeave(result) {
                emit('sdk-leave', {name, result: result.toInt32(), elapsedMs: Date.now() - this.start,
                    status: this.status && result.toInt32() === 0 ? u32(this.status) : null});
            }
        });
    }
});
emit('trace-ready', {});
