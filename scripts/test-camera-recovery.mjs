import assert from 'node:assert/strict';
import fs from 'node:fs';
import {recoveryFields,parseRecoveryValue,readRecovery} from '../crates/regain-alpaca/web/camera-recovery.mjs';

const description=JSON.parse(fs.readFileSync(new URL('../contracts/camera-recovery.json',import.meta.url)));
const all=recoveryFields(description,'linux'), windows=recoveryFields(description,'windows');
assert.equal(all.length,14); assert.equal(windows.length,13);
assert(!windows.some(f=>f.key==='usbPortCycle'));
assert.deepEqual(all.map(f=>f.key), ['maxRetries','maximumRetryExposureSeconds','reconnectDelaySeconds',
  'commandTimeoutSeconds','downloadTimeoutSeconds','exposureGraceSeconds','coolingTimeoutSeconds',
  'temperatureToleranceC','coolingStableSamples','coolingSampleSeconds','readyFrameDownloadRetries',
  'directReadRetries','usbResetAfterFailures','usbPortCycle']);
assert.throws(()=>recoveryFields({...description,contractVersion:2},'windows'));
for(const f of all) {
  assert(f.description); assert(['Recovery','Cooling','Timeouts'].includes(f['x-regain'].section));
  assert.equal(parseRecoveryValue(f,f.type==='boolean'?f.default:String(f.default)),f.default);
  if(f.type==='boolean') {assert.throws(()=>parseRecoveryValue(f,'false'));continue;}
  for(const v of ['', 'NaN','Infinity','-Infinity',String(f.maximum+1)]) assert.throws(()=>parseRecoveryValue(f,v));
  assert.equal(parseRecoveryValue(f,String(f.maximum)),f.maximum);
  if(f.exclusiveMinimum!==undefined) {
    assert.throws(()=>parseRecoveryValue(f,'0'));
    assert.equal(parseRecoveryValue(f,'0.000001'),0.000001);
  } else assert.equal(parseRecoveryValue(f,String(f.minimum)),f.minimum);
  if(f.type==='integer') assert.throws(()=>parseRecoveryValue(f,'1.5'));
}
const input=Object.fromEntries(windows.map(f=>[f.key,{value:String(f.default)}]));
input.reconnectDelaySeconds.value='0.000001';
const original={...description.schema.default,usbPortCycle:true,legacyExtension:'keep'};
const read=readRecovery(windows,original,{getElementById:key=>input[key]});
assert.equal(read.reconnectDelaySeconds,0.000001); assert.equal(read.usbPortCycle,true);
assert.equal(read.legacyExtension,'keep'); assert.equal(original.reconnectDelaySeconds,5);
const hub=JSON.parse(fs.readFileSync(new URL('../contracts/hub-config.json',import.meta.url)));
assert.deepEqual(description.schema,hub.schema.$defs.CameraRecovery);
console.log('Recovery metadata: ordered platform fields, legacy defaults, positive/integer limits, hidden values and hub parity pass.');
