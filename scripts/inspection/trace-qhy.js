// Passive research instrumentation in the owned SDK probe only.
// Record control payloads, never bulk image contents. No argument/result changes.
'use strict';
let sequence = 0;
const pending = new Map();
const paths = new Map();
const hooked = new Set();
function emit(kind, fields) { send({kind, timeMs:Date.now(), thread:Process.getCurrentThreadId(), ...fields}); }
function hex(p,n) { try {return p.isNull()?null:Array.from(new Uint8Array(p.readByteArray(n)),v=>v.toString(16).padStart(2,'0')).join('');}catch(_){return null;} }
function u32(p) {try{return p.isNull()?null:p.readU32();}catch(_){return null;} }
function hook(module,name,callbacks) {
    const address=module.findExportByName(name);
    if(address && !hooked.has(address.toString())) {hooked.add(address.toString());Interceptor.attach(address,callbacks);}
}
function capture(r,actual) {
    // Cypress control requests store payload at offset38, per supplied CyAPI ABI.
    if (r.code==='0x220020' && r.inputLength>=38 && r.inputLength<=65574)
        emit('control-complete',{sequence:r.sequence,header:hex(r.input,38),payload:hex(r.input.add(38),r.inputLength-38)});
    if(globalThis.QHY_CAPTURE_WIRE && r.code==='0x22004b' && actual>0 && actual<=r.outputLength)
        send({kind:'wire-bytes',sequence:r.sequence,endpoint:r.input.add(13).readU8(),bytes:actual},r.output.readByteArray(actual));
}
for(const moduleName of ['kernelbase.dll']) {
 const module=Process.getModuleByName(moduleName);
 for(const name of ['CreateFileA','CreateFileW']) hook(module,name,{
    onEnter(args){try{this.path=name.endsWith('W')?args[0].readUtf16String():args[0].readCString();}catch(_){}},
    onLeave(result){if(this.path && !result.equals(ptr(-1))){paths.set(result.toString(),this.path);emit('file-open',{path:this.path,handle:result.toString()});}}
 });
 hook(module,'DeviceIoControl',{
    onEnter(a){
        this.r={sequence:++sequence,handle:a[0].toString(),path:paths.get(a[0].toString()),code:'0x'+a[1].toUInt32().toString(16),input:a[2],output:a[4],inputLength:a[3].toUInt32(),outputLength:a[5].toUInt32(),overlapped:a[7].toString(),start:Date.now()};
        this.bytes=a[6];
        const r=this.r;
        emit('io-submit',{...r,input:r.input.toString(),output:r.output.toString(),header:hex(r.input,Math.min(38,r.inputLength))});
        if(r.code==='0x220020' && r.inputLength>=38 && r.inputLength<=65574 && !(r.input.readU8()&128))
            emit('control-output',{sequence:r.sequence,payload:hex(r.input.add(38),r.inputLength-38)});
        if(!a[7].isNull()) pending.set(r.handle+':'+r.overlapped,r);
    },
    onLeave(result){const r=this.r;emit('io-return',{sequence:r.sequence,ok:!!result.toInt32(),lastError:this.lastError,bytes:u32(this.bytes),elapsedMs:Date.now()-r.start});
        if(result.toInt32())capture(r,u32(this.bytes));
        if(result.toInt32()||this.lastError!==997)pending.delete(r.handle+':'+r.overlapped);
    }
 });
 hook(module,'GetOverlappedResult',{
    onEnter(a){this.key=a[0]+':'+a[1];this.r=pending.get(this.key);this.bytes=a[2];},
    onLeave(result){if(!this.r)return;const r=this.r;emit('io-complete',{sequence:r.sequence,ok:!!result.toInt32(),lastError:this.lastError,bytes:u32(this.bytes),elapsedMs:Date.now()-r.start,header:hex(r.input,Math.min(38,r.inputLength))});
        if(result.toInt32())capture(r,u32(this.bytes));
        if(result.toInt32()||this.lastError!==996)pending.delete(this.key);
    }
 });
}
Process.attachModuleObserver({onAdded(module){
 if(module.name.toLowerCase()!=='qhyccd.dll')return;
 for(const entry of module.enumerateExports()) {
    if(entry.type!=='function'||!(/QHYCCD/.test(entry.name)))continue;
    hook(module,entry.name,{
        onEnter(args){this.start=Date.now();emit('sdk-enter',{name:entry.name,args:[0,1,2,3].map(i=>args[i].toString())});},
        onLeave(result){emit('sdk-leave',{name:entry.name,result:result.toString(),elapsedMs:Date.now()-this.start});}
    });
 }
}});
