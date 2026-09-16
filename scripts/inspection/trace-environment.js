// Passive observation of environment registers in an owned camera worker.
// No image, identifier, firmware or calibration bytes are collected.
'use strict';
const pendingEnvironment = new Map();
let environmentSequence = 0;
function finishEnvironment(record, ok) {
    if (!ok) return;
    send({kind: 'environment-usb', sequence: record.sequence, timeMs: Date.now(),
        request: record.request, register: record.register, value: record.value,
        reply: record.request === 0xbd ? null :
            Array.from(new Uint8Array(record.input.add(38).readByteArray(record.size))),
        ntStatus: record.input.add(14).readU32(), usbStatus: record.input.add(18).readU32()});
}
const environmentKernel = Process.getModuleByName('kernel32.dll');
Interceptor.attach(environmentKernel.getExportByName('DeviceIoControl'), {
    onEnter(args) {
        if (args[1].toUInt32() !== 0x220020 || args[3].toUInt32() < 38) return;
        const input = args[2], request = input.add(1).readU8();
        const register = input.add(2).readU16(), size = input.add(6).readU16();
        if (!(request === 0xb3 && size === 2) &&
            !([0xbc, 0xbd].includes(request) && [0x19, 0x26, 0x2a, 0xfa, 0xfb].includes(register))) return;
        if (size > 2 || args[3].toUInt32() < 38 + size) return;
        this.record = {sequence: ++environmentSequence, input, request, register,
            value: input.add(4).readU16(), size};
        this.key = args[0] + ':' + args[7];
        if (!args[7].isNull()) pendingEnvironment.set(this.key, this.record);
    },
    onLeave(result) {
        if (!this.record) return;
        finishEnvironment(this.record, !!result.toInt32());
        if (result.toInt32() || this.lastError !== 997) pendingEnvironment.delete(this.key);
    }
});
Interceptor.attach(environmentKernel.getExportByName('GetOverlappedResult'), {
    onEnter(args) { this.key = args[0] + ':' + args[1]; },
    onLeave(result) {
        const record = pendingEnvironment.get(this.key);
        if (!record) return;
        finishEnvironment(record, !!result.toInt32());
        if (result.toInt32() || this.lastError !== 996) pendingEnvironment.delete(this.key);
    }
});
