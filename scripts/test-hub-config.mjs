import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { configurationContract } from '../crates/regain-alpaca/web/hub-config.mjs';
import { initialValue, newIdentity, previewValue, renderConfiguration } from '../crates/regain-alpaca/web/hub-form.mjs';
import { CredentialSetup, credentialContract } from '../crates/regain-alpaca/web/hub-credentials.mjs';
import { SimulationSetup, simulationControls, validateSimulationValue, parseSimulationArray } from '../crates/regain-alpaca/web/hub-simulation.mjs';
import { OutputDiagnostics, diagnosticSummary, validateDiagnosticSchema } from '../crates/regain-alpaca/web/hub-diagnostics.mjs';

const description = JSON.parse(readFileSync(new URL('../contracts/hub-config.json', import.meta.url), 'utf8'));
const reader = configurationContract(description);
const recoverySchema = reader.root.$defs.CameraRecovery;
const recoveryFields = reader.fields(recoverySchema);
assert.equal(recoveryFields.length, 14);
assert.equal(recoveryFields.find(f => f.key === 'maxRetries').value, 3);
assert.equal(recoveryFields.find(f => f.key === 'downloadTimeoutSeconds').value, 60);
assert.equal(recoveryFields.find(f => f.key === 'usbPortCycle').value, false);
assert.equal(recoveryFields.find(f => f.key === 'reconnectDelaySeconds').schema.exclusiveMinimum, 0);
assert.equal(recoveryFields.every(f => f.description.length > 0), true);
assert.match(recoveryFields.find(f => f.key === 'directReadRetries').description, /device-specific/);
const source = reader.root.$defs.SourceBackend;
const nativeWheel = reader.variants(source, ['nativeSources']).find(v => v.kind === 'native');
const nativeFields = reader.fields(nativeWheel.schema, {kind:'native', device:'efw', identity:'PRIVATE-WHEEL'});
const filterField = nativeFields.find(field => field.key === 'filterWheel');
assert.equal(filterField.required, false);
const filterSchema = reader.resolve(filterField.schema.anyOf.find(schema => schema.type !== 'null'));
const wheelNames = reader.resolve(filterSchema.properties.names);
const wheelOffsets = reader.resolve(filterSchema.properties.focusOffsets);
assert.deepEqual([wheelNames.minItems,wheelNames.maxItems], [1,1024]);
assert.deepEqual([wheelOffsets.minItems,wheelOffsets.maxItems], [1,1024]);
assert.equal(initialValue(reader, wheelOffsets.items), 0);
assert.deepEqual([wheelOffsets.items.minimum,wheelOffsets.items.maximum], [-2147483648,2147483647]);
assert.deepEqual(wheelOffsets.contains, {const:0});
assert.deepEqual(previewValue(reader, nativeWheel.schema, {kind:'native',device:'efw',identity:'PRIVATE-WHEEL',
  filterWheel:{names:['L','Hα',''],focusOffsets:[0,-12,17]}}).filterWheel, {names:['L','Hα',''],focusOffsets:[0,-12,17]});
const variants = reader.variants(source, ['alpacaSources']);
assert.equal(variants.find(v => v.kind === 'alpaca').enabled, true);
assert.equal(variants.find(v => v.kind === 'com').enabled, false);
const com = reader.variants(source, ['comSources']).find(v => v.kind === 'com');
assert.equal(com.enabled, true);
assert.deepEqual(reader.choices(com.schema.properties.deviceType).filter(c => c.enabled).map(c => c.value), ['switch','safetymonitor','observingconditions','focuser','rotator','filterwheel','covercalibrator']);
assert.equal(reader.choices(com.schema.properties.bitness, ['comX86Sources']).find(c => c.value === 'x86').enabled, true);
assert.equal(reader.choices(com.schema.properties.bitness, ['comX86Sources']).find(c => c.value === 'x64').enabled, false);
const fields = reader.fields(source, { kind: 'alpaca', baseUrl: 'http://localhost:11111', deviceNumber: 0 });
assert.equal(fields.some(f => f.key === 'progId'), false);
assert.equal(fields.find(f => f.key === 'baseUrl').value, 'http://localhost:11111');
assert.equal(fields.find(f => f.key === 'connectionPolicy').value, 'externallyManaged');
const connectionPolicy = fields.find(f => f.key === 'connectionPolicy').schema;
assert.deepEqual(reader.variants(connectionPolicy), []);
assert.deepEqual(reader.fields(connectionPolicy, 'externallyManaged'), []);
assert.deepEqual(reader.choices(connectionPolicy).map(v => v.value), ['externallyManaged', 'managed']);
assert.equal(reader.choices(connectionPolicy).every(v => v.description.length > 0), true);
const {default: ignoredPolicyDefault, ...policyWithoutDefault} = connectionPolicy;
assert.equal(initialValue(reader, policyWithoutDefault), 'externallyManaged');
assert.equal(previewValue(reader, connectionPolicy, 'managed'), 'managed');
assert.equal(initialValue(reader, {oneOf:[{const:'disabled','x-regain':{requiresCapability:'comSources'}},{const:'enabled'}]}), 'enabled');
assert.equal(reader.fields(reader.root).some(f => f.key === 'identities'), false);
assert.equal(reader.fields(reader.root).find(f => f.key === 'revision').readOnly, true);
const output = reader.root.$defs.OutputConfig;
assert.equal(reader.fields(output).find(f => f.key === 'number').readOnly, true);
assert.equal(reader.fields(output, {}, [], true).find(f => f.key === 'number').readOnly, false);
const policy = reader.fields(reader.root.$defs.SafetyPolicy);
assert.equal(policy.find(f => f.key === 'maximumSafeAgeSeconds').value, 90);
assert.equal(policy.find(f => f.key === 'maximumSafeAgeSeconds').schema['x-regain'].units, 's');
assert.equal(policy.find(f => f.key === 'safeReadingsToSafe').schema.type, 'integer');
assert.throws(() => configurationContract({ ...description, contractVersion: 2 }));
assert.throws(() => reader.resolve({ $ref: '#/$defs/missing' }));
console.log('Web hub configuration contract passed: conditional fields, capability gates, defaults, units, identities and protocol version.');

const editor = configurationContract({...description, capabilities:['alpacaSources','simulation']});
const created = initialValue(editor, editor.root.$defs.SourceConfig, () => '11111111-1111-4111-8111-111111111111');
assert.equal(created.id, '11111111-1111-4111-8111-111111111111');
assert.equal(created.backend.kind, 'alpaca');
assert.equal(created.backend.connectionPolicy, 'externallyManaged');
assert.deepEqual(initialValue(editor, editor.root.$defs.SafetyMember).policy, editor.root.$defs.SafetyMember.properties.policy.default);
assert.match(newIdentity(), /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
assert.notEqual(newIdentity(), newIdentity());
const saved = JSON.parse(readFileSync(new URL('../crates/regain-hub/examples/mixed-switch.json', import.meta.url), 'utf8'));
const privateSource = saved.sources.find(source => source.backend.kind === 'alpaca');
privateSource.backend.credentialReference = 'private-reference';
saved.identities = {sources: {'hidden':'ledger'}};
const preview = previewValue(editor, editor.root, saved);
assert.equal(preview.identities, undefined);
assert.equal(preview.sources.find(source => source.backend.kind === 'alpaca').backend.credentialReference, undefined);
assert.equal(privateSource.backend.credentialReference, 'private-reference');
assert.deepEqual(preview.outputs, saved.outputs);
assert.equal(editor.variants(editor.root.$defs.VirtualDevice).find(v => v.kind === 'proxy').enabled, false);
const typedEditor = configurationContract({...description, capabilities:['simulation','proxyOutputs','focuserOutputs','rotatorOutputs','filterWheelOutputs','coverCalibratorOutputs']});
const proxy = typedEditor.variants(typedEditor.root.$defs.VirtualDevice).find(v => v.kind === 'proxy');
assert.equal(proxy.enabled,true);
assert.deepEqual(typedEditor.choices(proxy.schema.properties.deviceType).filter(v => v.enabled).map(v => v.value),['focuser','rotator','filterwheel','covercalibrator']);
assert.deepEqual(reader.choices(proxy.schema.properties.deviceType,['focuserOutputs']).filter(v => v.enabled).map(v => v.value),['focuser']);
assert.deepEqual(reader.choices(proxy.schema.properties.deviceType,['rotatorOutputs']).filter(v => v.enabled).map(v => v.value),['rotator']);
assert.deepEqual(reader.choices(proxy.schema.properties.deviceType,['filterWheelOutputs']).filter(v => v.enabled).map(v => v.value),['filterwheel']);
assert.deepEqual(reader.choices(proxy.schema.properties.deviceType,['coverCalibratorOutputs']).filter(v => v.enabled).map(v => v.value),['covercalibrator']);
assert.deepEqual(initialValue(typedEditor,proxy.schema),{kind:'proxy',deviceType:'focuser',source:''});
assert.equal(reader.choices(proxy.schema.properties.deviceType).every(v => !v.enabled),true);
assert.deepEqual(typedEditor.choices(typedEditor.variants(typedEditor.root.$defs.SourceBackend).find(v => v.kind === 'simulated').schema.properties.deviceType).filter(v => v.enabled).map(v => v.value),['switch','safetymonitor','observingconditions','focuser','rotator','filterwheel','covercalibrator']);
const simulatedSchema = editor.variants(editor.root.$defs.SourceBackend).find(v => v.kind === 'simulated').schema;
assert.equal(initialValue(editor, simulatedSchema).deviceType,'switch');
assert.equal(editor.choices(simulatedSchema.properties.deviceType).find(v=>v.value==='camera').enabled,false);
console.log('Web hub draft helpers passed: identities, capability choices, shared defaults, non-mutating metadata-driven redaction.');

const storage = {protection:'userFilePermissions',protectionDescription:'Private files; not encrypted',clientChosenReferences:true,referencePrefix:'credential-',rotation:'Create, apply, remove',
  reference:{type:'string',label:'Reference',description:'Separate storage handle',maxLength:200},
  input:{authorization:{type:'string',label:'Authorization',description:'Complete header',maxLength:8192,writeOnly:true,sensitive:true}}};
assert.equal(credentialContract(null),null);
assert.throws(() => credentialContract({...storage,input:{authorization:{...storage.input.authorization,writeOnly:false}}}));
const id = '11111111-1111-4111-8111-111111111111';
for (const failure of ['lost','malformed','unavailable']) {
  let writes = 0, stored, visible, review = true;
  const setup = new CredentialSetup(async command => {
    if (command.op === 'createCredential') {
      writes++; assert.equal(visible, 'credential-' + id); stored = visible;
      if (failure === 'lost') throw new Error('Reply lost');
      if (failure === 'unavailable') { const error = new Error('Storage unavailable'); error.detail = {code:'unavailable'}; throw error; }
      return {reference:stored,present:true,protection:storage.protection,authorization:'forbidden-value'};
    }
    assert.equal(command.op,'credentialStatus'); return {reference:stored,present:true,protection:storage.protection};
  }, value => visible = value, () => review = false, () => id);
  setup.load(storage);
  await assert.rejects(setup.create('Bearer private-js-fixture'), error => !error.message.includes('private-js-fixture') && !error.message.includes('forbidden-value'));
  assert.equal(review,false); assert.equal(setup.uncertain,true); assert.equal(setup.reference,stored);
  await assert.rejects(setup.create('Bearer another-value')); await assert.rejects(setup.remove()); assert.equal(writes,1);
  setup.load(storage); assert.equal(setup.reference,stored); assert.equal((await setup.status()).present,true); assert.equal(writes,1);
  assert.equal(JSON.stringify(setup).includes('private-js-fixture'),false);
}
let writes = 0, release;
const pending = new CredentialSetup(async () => { writes++; return await new Promise(resolve => release = resolve); },()=>{},()=>{},()=>id);
pending.load(storage);
await assert.rejects(pending.create('Bearer invalid\r\nvalue')); assert.equal(writes,0); assert.equal(pending.uncertain,false);
const writing = pending.create('Bearer pending-fixture');
await assert.rejects(pending.create('Bearer competing-fixture')); await assert.rejects(pending.status());
assert.throws(() => pending.load(storage)); assert.throws(() => pending.setReference('different')); assert.equal(writes,1);
release({reference:'credential-' + id,present:true,protection:storage.protection}); await writing;
pending.setReference('😀'.repeat(200)); pending.validateReference();
pending.setReference('😀'.repeat(201)); assert.throws(() => pending.validateReference());
pending.setReference('\ud800'); assert.throws(() => pending.validateReference());
pending.load({...storage,clientChosenReferences:false}); await assert.rejects(pending.create('Bearer unsupported-fixture')); assert.equal(writes,1);
console.log('Web credentials passed: retained references, lost/malformed/storage failures, strict replies, no replay, review invalidation, admission and scalar limits.');

const simDescription = description.simulationControl;
const simulatedSource = type => ({id:'11111111-1111-4111-8111-111111111111',backend:{kind:'simulated',deviceType:type}});
const simRevision = '22222222-2222-4222-8222-222222222222';
function simulationState(type) {
  const state = {deviceType:type,safe:false,switchValues:{},weather:{},fault:'none',sampleAgeSeconds:0};
  for (const control of simulationControls(simDescription,simulatedSource(type))) {
    if (control.path.length === 1) state[control.path[0]] = control.default;
    else { state[control.path[0]] ??= {}; state[control.path[0]][control.path[1]] = control.default; }
  }
  return state;
}
for (const type of ['switch','safetymonitor','observingconditions','focuser','rotator','filterwheel','covercalibrator']) {
  const fields = simulationControls(simDescription,simulatedSource(type));
  assert.equal(fields.length,type==='switch'?5:type==='safetymonitor'?2:type==='rotator'?12:type==='filterwheel'?6:type==='covercalibrator'?10:15);
  assert.deepEqual(fields.find(f=>f.path[0]==='fault').enum,simDescription.faultsByDeviceType[type]);
}
assert.deepEqual(simulationControls(simDescription,{backend:{kind:'native'}}),[]);
{
  const fields=simulationControls(simDescription,simulatedSource('covercalibrator'));
  const brightness=fields.find(f=>f.path[1]==='brightness'), cover=fields.find(f=>f.path[1]==='coverState');
  for(const value of [-1,2147483648,1.5,'0',true]) assert.throws(()=>validateSimulationValue(brightness,value));
  for(const value of [-1,6,1.5,'1']) assert.throws(()=>validateSimulationValue(cover,value));
  const state=simulationState('covercalibrator'); let writes=0;
  const setup=new SimulationSetup(async command=>{
    writes++; assert.deepEqual(command.update,{coverCalibrator:{coverState:4,coverMoving:false}});
    Object.assign(state.coverCalibrator,command.update.coverCalibrator);
    return {source:setup.source.id,configurationRevision:simRevision,simulation:state};
  },()=>{});
  setup.load(simDescription,simulatedSource('covercalibrator'),simRevision);
  await assert.rejects(setup.update([{path:['coverCalibrator','coverState'],value:6}])); assert.equal(writes,0);
  assert.equal((await setup.update([{path:['coverCalibrator','coverState'],value:4},{path:['coverCalibrator','coverMoving'],value:false}])).coverCalibrator.coverState,4);
  for(const mutate of [s=>s.coverCalibrator.brightness=4097,s=>s.coverCalibrator.brightness=1,
    s=>{s.coverCalibrator.coverState=0;s.coverCalibrator.coverMoving=true;},
    s=>{s.coverCalibrator.calibratorState=0;s.coverCalibrator.calibratorChanging=true;},
    s=>s.coverCalibrator.extra=true,s=>delete s.coverCalibrator.coverMoving]) {
    const bad=structuredClone(state); mutate(bad); assert.throws(()=>setup.statusValue(bad));
  }
}
{
  const fields = simulationControls(simDescription,simulatedSource('filterwheel'));
  const names = fields.find(f=>f.path[1]==='names'), offsets = fields.find(f=>f.path[1]==='focusOffsets');
  for(const value of [[],[0],['\ud800'],['x'.repeat(names.maxUtf8Bytes+1)],Array(1025).fill('')]) assert.throws(()=>validateSimulationValue(names,value));
  for(const value of [[],[1],['0'],[0,1.5],[0,2147483648],[0,-2147483649],Array(1025).fill(0)]) assert.throws(()=>validateSimulationValue(offsets,value));
  assert.deepEqual(validateSimulationValue(names,['L','Hα','😀','']),['L','Hα','😀','']);
  assert.deepEqual(validateSimulationValue(offsets,[-2147483648,0,2147483647]),[-2147483648,0,2147483647]);
  for(const text of ['', '[', 'L,R', '{}','null','[0]']) assert.throws(()=>parseSimulationArray(names,text),error=>error.detail.code==='invalidValue');
  assert.deepEqual(parseSimulationArray(names,'["L","Hα",""]'),['L','Hα','']);
  assert.deepEqual(parseSimulationArray(offsets,'[-12,0,17]'),[-12,0,17]);
  let requests=0; const state=simulationState('filterwheel');
  const setup=new SimulationSetup(async command=>{
    requests++; assert.deepEqual(command.update,{filterWheel:{names:['L','Hα',''],focusOffsets:[-12,0,17]}});
    Object.assign(state.filterWheel,command.update.filterWheel);
    return {source:setup.source.id,configurationRevision:simRevision,simulation:state};
  },()=>{});
  setup.load(simDescription,simulatedSource('filterwheel'),simRevision);
  await assert.rejects(setup.update([{path:['filterWheel','focusOffsets'],value:[1]}])); assert.equal(requests,0);
  assert.deepEqual((await setup.update([{path:['filterWheel','names'],value:['L','Hα','']},{path:['filterWheel','focusOffsets'],value:[-12,0,17]}])).filterWheel.focusOffsets,[-12,0,17]);
  for(const mutate of [s=>s.filterWheel.names=[],s=>s.filterWheel.focusOffsets=[0],s=>s.filterWheel.position=3,
    s=>s.filterWheel.position=-2,s=>s.filterWheel.focusOffsets=[0,2147483648,17],s=>s.filterWheel.extra=0,s=>delete s.filterWheel.names]) {
    const malformed=structuredClone(state); mutate(malformed); assert.throws(()=>setup.statusValue(malformed));
  }
}
const focuserFields = simulationControls(simDescription,simulatedSource('focuser'));
const positionField = focuserFields.find(field => field.path[1] === 'position');
assert.equal(positionField.type,'integer');
for (const value of [1.5,'1',-1,2147483648,Infinity]) assert.throws(() => validateSimulationValue(positionField,value));
assert.equal(validateSimulationValue(positionField,100000),100000);
let focuserWrites = 0;
const focuserState = simulationState('focuser');
const focuserSetup = new SimulationSetup(async command => {
  focuserWrites++; assert.deepEqual(command.update,{focuser:{position:100000}});
  focuserState.focuser.position = 100000;
  return {source:focuserSetup.source.id,configurationRevision:simRevision,simulation:focuserState};
},()=>{});
focuserSetup.load(simDescription,simulatedSource('focuser'),simRevision);
await assert.rejects(focuserSetup.update([{path:['focuser','position'],value:1.5}])); assert.equal(focuserWrites,0);
assert.equal((await focuserSetup.update([{path:['focuser','position'],value:100000}])).focuser.position,100000);
const invalidFocuserState = structuredClone(focuserState); invalidFocuserState.focuser.extra = true;
assert.throws(() => focuserSetup.statusValue(invalidFocuserState));
const rotatorFields = simulationControls(simDescription,simulatedSource('rotator'));
const angleControl = rotatorFields.find(f=>f.path[1]==='position');
for (const value of [-1,360,359.9999999,'30',1e39,Infinity]) assert.throws(()=>validateSimulationValue(angleControl,value));
assert.equal(validateSimulationValue(angleControl,359.9999694824219),359.9999694824219);
const stepControl = rotatorFields.find(f=>f.path[1]==='stepSize');
for (const value of [0,1e-300,1e39]) assert.throws(()=>validateSimulationValue(stepControl,value));
assert.equal(validateSimulationValue(stepControl,0.02),0.02);
let rotatorWrites = 0;
const rotatorState = simulationState('rotator');
const rotatorSetup = new SimulationSetup(async command => {
  rotatorWrites++; assert.deepEqual(command.update,{rotator:{position:20,mechanicalPosition:350}});
  Object.assign(rotatorState.rotator,command.update.rotator);
  return {source:rotatorSetup.source.id,configurationRevision:simRevision,simulation:rotatorState};
},()=>{});
rotatorSetup.load(simDescription,simulatedSource('rotator'),simRevision);
await assert.rejects(rotatorSetup.update([{path:['rotator','position'],value:359.9999999}])); assert.equal(rotatorWrites,0);
assert.equal((await rotatorSetup.update([{path:['rotator','position'],value:20},{path:['rotator','mechanicalPosition'],value:350}])).rotator.position,20);
for (const mutate of [state=>state.rotator.position=360,state=>state.rotator.stepSize=1e-300,
    state=>state.rotator.extra=true,state=>delete state.rotator.reverse,state=>state.focuser={}]) {
  const state = structuredClone(rotatorState); mutate(state); assert.throws(()=>rotatorSetup.statusValue(state));
}
delete invalidFocuserState.focuser.extra; invalidFocuserState.focuser.position = 1.5;
assert.throws(() => focuserSetup.statusValue(invalidFocuserState));
const weatherFields = simulationControls(simDescription,simulatedSource('observingconditions'));
const pressure = weatherFields.find(f=>f.path[1]==='pressure');
assert.throws(()=>validateSimulationValue(pressure,0)); assert.equal(validateSimulationValue(pressure,null),null);
assert.throws(()=>validateSimulationValue(weatherFields.find(f=>f.path[1]==='temperature'),-273.16));
for (const failure of ['lost','wrongRevision','wrongSource','malformed','unavailable','responseTooLarge']) {
  let requests = 0, review = true; const state = simulationState('switch');
  const source = simulatedSource('switch');
  const setup = new SimulationSetup(async command => {
    if (command.op==='sourceStatus') return {source:source.id,revision:simRevision,simulated:true,simulation:state};
    requests++; assert.equal(command.expectedRevision,simRevision); assert.deepEqual(command.update,{switchValues:{1:42}});
    state.switchValues[1]=42;
    if (failure==='lost') throw new Error('Lost reply');
    if (failure==='unavailable' || failure==='responseTooLarge') { const error = new Error('Unavailable reply'); error.detail={code:failure}; throw error; }
    const result = {source:source.id,configurationRevision:simRevision,simulation:structuredClone(state)};
    if (failure==='wrongRevision') result.configurationRevision=source.id;
    if (failure==='wrongSource') result.source=simRevision;
    if (failure==='malformed') result.simulation.fault='unexpected';
    return result;
  },()=>review=false);
  setup.load(simDescription,source,simRevision);
  await assert.rejects(setup.update([{path:['switchValues','1'],value:42}])); assert.equal(setup.uncertain,true); assert.equal(review,false);
  await assert.rejects(setup.update([{path:['switchValues','1'],value:42}])); await assert.rejects(setup.read()); assert.equal(requests,1);
  setup.load(simDescription,source,simRevision); assert.equal((await setup.read()).switchValues[1],42); assert.equal(requests,1);
}
let simWrites=0, finishSimulation;
const sim = new SimulationSetup(async command => {simWrites++; return await new Promise(resolve=>finishSimulation=resolve);},()=>{});
sim.load(simDescription,simulatedSource('switch'),simRevision);
await assert.rejects(sim.update([])); await assert.rejects(sim.update([{path:['switchValues','1'],value:101}]));
await assert.rejects(sim.update([{path:['safe'],value:true}])); assert.equal(simWrites,0); assert.equal(sim.uncertain,false);
const changing = sim.update([{path:['switchValues','1'],value:7}]);
await assert.rejects(sim.read()); await assert.rejects(sim.update([{path:['switchValues','1'],value:8}]));
assert.throws(()=>sim.load(simDescription,simulatedSource('switch'),simRevision)); assert.equal(simWrites,1);
const finalSim = simulationState('switch'); finalSim.switchValues[1]=7;
finishSimulation({source:sim.source.id,configurationRevision:simRevision,simulation:finalSim}); await changing;
sim.load(simDescription,simulatedSource('observingconditions'),simRevision);
assert.deepEqual(sim.patch([{path:['weather','temperature'],value:null},{path:['sampleAgeSeconds'],value:60}]),{weather:{temperature:null},sampleAgeSeconds:60});
assert.throws(()=>sim.patch([{path:['weather','temperature'],value:3},{path:['weather','temperature'],value:4}]));
console.log('Web simulation passed: shared descriptors, sparse updates, physical limits, sensor absence, revision fencing, lost/malformed replies, no replay and operation admission.');

const diagSaved=JSON.parse(readFileSync(new URL('../crates/regain-hub/examples/simulated-observatory.json',import.meta.url),'utf8'));
// getConfig serializes defaulted fields; use that wire shape in reply fixtures.
for (const output of diagSaved.outputs) {
  for (const channel of output.device.channels??[]) channel.readout.unit??=null;
  for (const measurement of Object.values(output.device.measurements??{})) {
    for (const field of reader.fields(reader.root.$defs.Measurement,measurement)) if (!Object.hasOwn(measurement,field.key)) measurement[field.key]=field.value;
    for (const readout of measurement.sources) readout.unit??=null;
  }
}
const diagGeneration='77777777-7777-4777-8777-777777777777';
const diagHealth=source=>({source,revision:diagSaved.revision,generation:diagGeneration,sequence:0,transportConnected:false,writeUncertain:false,leaseCount:0,error:null,polling:{phase:'idle',observedSeconds:0,reason:null,nextPollAfterSeconds:null,attemptsStarted:0,attemptsPerCycle:description.schema.$defs.PollPolicy.properties.attemptsPerCycle.default,lastAttempt:0,lastCycleExhausted:null,backoffFailures:0}});
const diagUnavailable={state:'unavailable',error:{kind:'disconnected',message:'Source is disconnected',upstreamCode:null}};
const focuserSaved=structuredClone(diagSaved);
focuserSaved.sources=[{...focuserSaved.sources[0],backend:{kind:'native',device:'fc3',identity:'00:00:00:00:00:03'}}];
focuserSaved.identities={outputs:{},channels:{}};
focuserSaved.outputs=[{...focuserSaved.outputs[0],device:{kind:'proxy',deviceType:'focuser',source:focuserSaved.sources[0].id}}];
const focuserOutput=focuserSaved.outputs[0];
function focuserReply(start=0,limit=1) {
  const fields=description.outputDiagnostics.focuserProperties, total=fields.length, health=diagHealth(focuserSaved.sources[0].id);
  health.transportConnected=true; health.leaseCount=1;
  const properties=fields.slice(start,start+limit).map(field=>({property:field.property,sample:{state:'available',reading:{
    value:{type:field.valueType,value:field.valueType==='boolean'?true:1},ageSeconds:0.5,source:health.source,generation:health.generation,sequence:0,revision:focuserSaved.revision}}}));
  const end=Math.min(start+limit,total);
  return {purpose:'cachedDiagnostics',output:focuserOutput.id,configurationRevision:focuserSaved.revision,observedSeconds:1,
    deviceType:'focuser',simulated:true,start,limit,total,nextStart:end<total?end:null,diagnostics:{kind:'focuser',health,properties}};
}
{
  const setup=new OutputDiagnostics(async command=>focuserReply(command.start,command.limit),()=>assert.fail('Review revoked'));
  setup.load(description,focuserSaved);
  assert.equal((await setup.read(focuserOutput.id,0,4)).diagnostics.properties.length,4);
  const result=await setup.read(focuserOutput.id,4,32); assert.equal(result.nextStart,null);
  assert.match(diagnosticSummary(result),/position: 1/); assert.match(diagnosticSummary(result),/age 0.5 s/);
}
for (const fault of ['property','source','generation','sequence','type','minimum','extra','age']) {
  let revoked=false;
  const setup=new OutputDiagnostics(async()=>{
    const reply=focuserReply(4,1), item=reply.diagnostics.properties[0], reading=item.sample.reading;
    if(fault==='property') item.property='isMoving';
    if(fault==='source') reading.source='88888888-8888-4888-8888-888888888888';
    if(fault==='generation') reading.generation='88888888-8888-4888-8888-888888888888';
    if(fault==='sequence') reading.sequence=1;
    if(fault==='type') reading.value={type:'number',value:1};
    if(fault==='minimum') reading.value.value=-1;
    if(fault==='extra') reading.authorization='PRIVATE_FORBIDDEN_REPLY';
    if(fault==='age') reading.ageSeconds=-1;
    return reply;
  },()=>revoked=true);
  setup.load(description,focuserSaved); await assert.rejects(setup.read(focuserOutput.id,4,1));
  assert.equal(revoked,true); assert.equal(setup.observation,null);
}
function diagReply(output,start=0,limit=1) {
  const device=output.device; let total, diagnostics;
  if(device.kind==='switch') {
    total=device.channels.length;
    diagnostics={kind:'switch',channels:device.channels.slice(start,start+limit).map(c=>({state:'configured',number:c.number,id:c.id,label:c.label,readout:c.readout,units:c.units,minimum:c.minimum,maximum:c.maximum,step:c.step,configuredWritable:c.writable,maximumAgeSeconds:31,sample:structuredClone(diagUnavailable),health:diagHealth(c.readout.source)}))};
  } else if(device.kind==='safety') {
    total=device.members.length;
    const policy=Object.fromEntries(Object.entries(description.schema.$defs.SafetyPolicy.properties).map(([key,field])=>[key,field.default]));
    diagnostics={kind:'safety',controllerActive:false,isSafe:false,members:device.members.slice(start,start+limit).map(m=>({source:m.source,enabled:m.enabled,policy,health:diagHealth(m.source),decision:{configurationRevision:diagSaved.revision,generation:diagGeneration,phase:'unknown',rawIsSafe:null,permitsSafe:false,recoveryConfirmed:false,safeAgeSeconds:null,failedCycles:0,unsafeReadings:0,safeReadings:0,safeHoldSeconds:0,reason:'No evidence',lastSequence:0}}))};
  } else {
    const metrics=Object.keys(device.measurements).sort(); total=metrics.length;
    diagnostics={kind:'weather',averagePeriodHours:0,measurements:metrics.slice(start,start+limit).map(metric=>({metric,configuration:device.measurements[metric],sample:structuredClone(diagUnavailable),sources:device.measurements[metric].sources.map(s=>diagHealth(s.source))}))};
  }
  const end=Math.min(start+limit,total);
  return {purpose:'cachedDiagnostics',output:output.id,configurationRevision:diagSaved.revision,observedSeconds:1,deviceType:{switch:'switch',safety:'safetymonitor',weather:'observingconditions'}[device.kind],simulated:true,start,limit,total,nextStart:end<total?end:null,diagnostics};
}
for(const output of diagSaved.outputs) {
  let review=true;
  const setup=new OutputDiagnostics(async(command,path,deadline)=>{assert.equal(command.op,'outputStatus');assert.equal(command.expectedRevision,diagSaved.revision);assert.equal(deadline,description.outputDiagnostics.deadlineSeconds);return diagReply(output,command.start,command.limit);},()=>review=false);
  setup.load(description,diagSaved); const reply=await setup.read(output.id,0,1);
  assert.equal(review,true); assert.equal(setup.observation.result.output,output.id);
  assert.equal(setup.observation.configurationRevision,diagSaved.revision);
  assert.equal(JSON.stringify(setup.observation).includes('backend'),false);
  const summary=diagnosticSummary(reply); assert.match(summary,/Simulation/);
  if(output.device.kind==='safety') {assert.match(summary,/UNSAFE/);assert.match(summary,/raw unknown/);}
  else assert.match(summary,/unavailable/);
}
for(const fault of ['lost','revision','output','cursor','channel','secretRoot','secretHealth','type','emptyGeneration','missingError','missingPolling','negativeWait','impossibleWait']) {
  let requests=0, review=true;
  const setup=new OutputDiagnostics(async command=>{
    requests++; if(fault==='lost') throw new Error('Lost reply');
    const reply=diagReply(diagSaved.outputs[0],command.start,command.limit);
    if(fault==='revision') reply.configurationRevision=diagGeneration;
    if(fault==='output') reply.output=diagGeneration;
    if(fault==='cursor') reply.nextStart=0;
    if(fault==='channel') reply.diagnostics.channels[0].id=diagGeneration;
    if(fault==='secretRoot') reply.authorization='PRIVATE_FORBIDDEN_REPLY';
    if(fault==='secretHealth') reply.diagnostics.channels[0].health.authorization='PRIVATE_FORBIDDEN_REPLY';
    if(fault==='type') reply.diagnostics.channels[0].minimum='0';
    if(fault==='emptyGeneration') reply.diagnostics.channels[0].health.generation='00000000-0000-0000-0000-000000000000';
    if(fault==='missingError') delete reply.diagnostics.channels[0].health.error;
    if(fault==='missingPolling') delete reply.diagnostics.channels[0].health.polling;
    if(fault==='negativeWait') reply.diagnostics.channels[0].health.polling.nextPollAfterSeconds=-1;
    if(fault==='impossibleWait') reply.diagnostics.channels[0].health.polling.nextPollAfterSeconds=10;
    return reply;
  },()=>review=false);
  setup.load(description,diagSaved);
  await assert.rejects(setup.read(diagSaved.outputs[0].id,0,1),error=>!error.message.includes('PRIVATE_FORBIDDEN_REPLY'));
  assert.equal(setup.uncertain,true);assert.equal(setup.observation,null);assert.equal(review,false);
  await assert.rejects(setup.read(diagSaved.outputs[0].id,0,1));assert.equal(requests,1);
  setup.load(description,diagSaved);assert.equal(requests,1);assert.equal(setup.uncertain,false);
}
let diagRequests=0, finishDiagnostic;
const diagPending=new OutputDiagnostics(async command=>{diagRequests++;return await new Promise(resolve=>finishDiagnostic=()=>resolve(diagReply(diagSaved.outputs[0],command.start,command.limit)));});
diagPending.load(description,diagSaved);
await assert.rejects(diagPending.read(diagGeneration,0,1));await assert.rejects(diagPending.read(diagSaved.outputs[0].id,0,0));assert.equal(diagRequests,0);
const observing=diagPending.read(diagSaved.outputs[0].id,0,1);
await assert.rejects(diagPending.read(diagSaved.outputs[0].id,1,1));assert.throws(()=>diagPending.load(description,diagSaved));assert.equal(diagRequests,1);
finishDiagnostic();await observing;
const notFinite=diagReply(diagSaved.outputs[0]);notFinite.observedSeconds=Infinity;
assert.throws(()=>validateDiagnosticSchema(description.outputDiagnostics.responseSchema,notFinite));
console.log('Web output diagnostics passed: generated reply schema, saved identity/revision/page fences, nested redaction, uncertainty/reload, admission and host decisions.');
// A $ref and its siblings both constrain a reply; neither can replace the other.
{
  const schema = {$defs:{value:{type:'number',minimum:2}},$ref:'#/$defs/value',maximum:3};
  validateDiagnosticSchema(schema,2);
  assert.throws(()=>validateDiagnosticSchema(schema,1));
  assert.throws(()=>validateDiagnosticSchema(schema,4));
}
// Presentation retains the actor's observed wait; it does not run a countdown.
{
  const reply=diagReply(diagSaved.outputs[0]);
  const polling=reply.diagnostics.channels[0].health.polling;
  Object.assign(polling,{phase:'waiting',reason:'retry',observedSeconds:30,nextPollAfterSeconds:8,attemptsStarted:1,lastAttempt:1,lastCycleExhausted:false,backoffFailures:1});
  const setup=new OutputDiagnostics(async()=>reply); setup.load(description,diagSaved);
  const observed=await setup.read(diagSaved.outputs[0].id,0,1);
  assert.match(diagnosticSummary(observed),/next poll scheduled in 8.000 s at that observation; actor work may delay it/);
  assert.match(diagnosticSummary(observed),/Read attempts started 1\/3/);
  assert.equal(setup.observation.result.diagnostics.channels[0].health.polling.nextPollAfterSeconds,8);
}

// Exercise the actual form's event closures. This small DOM adapter supplies only
// the element/tree operations used by the renderer; browser acceptance checks
// native controls and layout separately.
class FormElement {
  constructor(tag) { this.tagName=tag.toUpperCase(); this.children=[]; this.dataset={}; this.attributes={}; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children=children; }
  setAttribute(key,value) { this.attributes[key]=value; }
  setCustomValidity(value) { this.validityMessage=value; }
  descendants() { return this.children.flatMap(child => [child,...child.descendants()]); }
  querySelectorAll(selector) { assert.equal(selector,'details[open]'); return this.descendants().filter(child=>child.tagName==='DETAILS' && child.open); }
}
const priorDocument=globalThis.document;
try {
  globalThis.document={createElement:tag=>new FormElement(tag)};
  for (const deviceClass of ['rotator','filterwheel','covercalibrator']) {
    const container=new FormElement('div');
    const formReader=configurationContract({...description,capabilities:['alpacaSources','simulation','comSources','comX64Sources','proxyOutputs','focuserOutputs','rotatorOutputs','filterWheelOutputs','coverCalibratorOutputs']});
    const baseline=JSON.parse(readFileSync(new URL('../crates/regain-hub/examples/two-source-safety.json',import.meta.url),'utf8'));
    const draft=structuredClone(baseline); let changes=0;
    const field=path=>container.descendants().find(child=>child.dataset.path===path);
    const taggedChoice=path=>field(path).descendants().find(child=>child.tagName==='SELECT');
    renderConfiguration(container,formReader,draft,baseline,()=>changes++);
    let transport=taggedChoice('sources[0].backend'); transport.value='simulated'; transport.onchange();
    assert.deepEqual(draft.sources[0].backend,{kind:'simulated',deviceType:'switch'});
    assert.equal(field('sources[0].backend.baseUrl'),undefined);
    assert.notEqual(field('sources[0].backend.deviceType'),undefined);
    let deviceType=taggedChoice('sources[0].backend.deviceType'); deviceType.value=deviceClass; deviceType.onchange();
    assert.equal(draft.sources[0].backend.deviceType,deviceClass);
    transport=taggedChoice('sources[0].backend'); transport.value='com'; transport.onchange();
    assert.equal(draft.sources[0].backend.kind,'com'); assert.equal(draft.sources[0].backend.bitness,'x64');
    assert.notEqual(field('sources[0].backend.progId'),undefined);
    deviceType=taggedChoice('sources[0].backend.deviceType'); deviceType.value=deviceClass; deviceType.onchange();
    assert.equal(draft.sources[0].backend.deviceType,deviceClass);
    const outputChoice=taggedChoice('outputs[0].device'); outputChoice.value='proxy'; outputChoice.onchange();
    assert.deepEqual(draft.outputs[0].device,{kind:'proxy',source:'',deviceType:'focuser'});
    const classes=taggedChoice('outputs[0].device.deviceType').children.filter(child=>child.value);
    assert.deepEqual(classes.filter(child=>!child.disabled).map(child=>child.value),['focuser','rotator','filterwheel','covercalibrator']);
    deviceType=taggedChoice('outputs[0].device.deviceType'); deviceType.value=deviceClass; deviceType.onchange();
    assert.equal(draft.outputs[0].device.deviceType,deviceClass);
    assert.equal(changes,6); assert.equal(baseline.outputs[0].device.kind,'safety');
  }
} finally {
  if (priorDocument===undefined) delete globalThis.document; else globalThis.document=priorDocument;
}
console.log('Web form transitions passed: actual event handlers change transports and proxy kinds, replace conditional fields and restrict typed classes.');

// Rotator diagnostics share typed accessory validation but keep angular limits.
const rotatorSaved=structuredClone(focuserSaved);
rotatorSaved.sources[0].backend={kind:'native',device:'caa',identity:'0102030405060708'};
rotatorSaved.outputs[0].device.deviceType='rotator';
const rotatorOutput=rotatorSaved.outputs[0];
function rotatorReply(start=0,limit=1) {
  const fields=description.outputDiagnostics.rotatorProperties, total=fields.length, health=diagHealth(rotatorSaved.sources[0].id);
  health.transportConnected=true; health.leaseCount=1;
  const properties=fields.slice(start,start+limit).map(field=>({property:field.property,sample:{state:'available',reading:{
    value:{type:field.valueType,value:field.valueType==='boolean'?true:field.property==='stepSize'?0.02:42.5},
    ageSeconds:0.5,source:health.source,generation:health.generation,sequence:0,revision:rotatorSaved.revision}}}));
  const end=Math.min(start+limit,total);
  return {purpose:'cachedDiagnostics',output:rotatorOutput.id,configurationRevision:rotatorSaved.revision,observedSeconds:1,
    deviceType:'rotator',simulated:true,start,limit,total,nextStart:end<total?end:null,diagnostics:{kind:'rotator',health,properties}};
}
{
  const setup=new OutputDiagnostics(async command=>rotatorReply(command.start,command.limit),()=>assert.fail('Review revoked'));
  setup.load(description,rotatorSaved);
  const first=await setup.read(rotatorOutput.id,0,4); assert.equal(first.nextStart,4);
  assert.match(diagnosticSummary(first),/position: 42.5/); assert.match(diagnosticSummary(first),/age 0.5 s/);
  assert.equal((await setup.read(rotatorOutput.id,4,32)).nextStart,null);
}
for (const fault of ['property','source','generation','sequence','type','minimum','exclusiveMaximum','extra','age','stepZero','stepSingleRange']) {
  let revoked=false; const index=fault.startsWith('step')?5:3;
  const setup=new OutputDiagnostics(async()=>{
    const reply=rotatorReply(index,1), item=reply.diagnostics.properties[0], reading=item.sample.reading;
    if(fault==='property') item.property='isMoving';
    if(fault==='source') reading.source='88888888-8888-4888-8888-888888888888';
    if(fault==='generation') reading.generation='88888888-8888-4888-8888-888888888888';
    if(fault==='sequence') reading.sequence=1;
    if(fault==='type') reading.value={type:'integer',value:1};
    if(fault==='minimum') reading.value.value=-1;
    if(fault==='exclusiveMaximum') reading.value.value=360;
    if(fault==='extra') reading.authorization='PRIVATE_FORBIDDEN_REPLY';
    if(fault==='age') reading.ageSeconds=-1;
    if(fault==='stepZero') reading.value.value=0;
    if(fault==='stepSingleRange') reading.value.value=Number.MAX_VALUE;
    return reply;
  },()=>revoked=true);
  setup.load(description,rotatorSaved); await assert.rejects(setup.read(rotatorOutput.id,index,1));
  assert.equal(revoked,true); assert.equal(setup.observation,null);
}

// Wheel metadata remains ordered, signed and typed through the same reader.
const wheelSaved=structuredClone(focuserSaved);
wheelSaved.sources[0].backend={kind:'native',device:'efw',identity:'0102030405060708'};
wheelSaved.outputs[0].device.deviceType='filterwheel';
const wheelOutput=wheelSaved.outputs[0];
function wheelReply(start=0,limit=3) {
  const fields=description.outputDiagnostics.filterwheelProperties, health=diagHealth(wheelSaved.sources[0].id);
  health.transportConnected=true; health.leaseCount=1;
  const values={names:['L','Hα',''],focusOffsets:[-12,0,17],position:-1};
  const properties=fields.slice(start,start+limit).map(field=>({property:field.property,sample:{state:'available',reading:{
    value:{type:field.valueType,value:values[field.property]},ageSeconds:5,source:health.source,
    generation:health.generation,sequence:0,revision:wheelSaved.revision}}}));
  const end=Math.min(start+limit,fields.length);
  return {purpose:'cachedDiagnostics',output:wheelOutput.id,configurationRevision:wheelSaved.revision,observedSeconds:6,
    deviceType:'filterwheel',simulated:true,start,limit,total:fields.length,nextStart:end<fields.length?end:null,
    diagnostics:{kind:'filterwheel',health,properties}};
}
{
  const setup=new OutputDiagnostics(async command=>wheelReply(command.start,command.limit),()=>assert.fail('Review revoked'));
  setup.load(description,wheelSaved);
  const first=await setup.read(wheelOutput.id,0,2); assert.equal(first.nextStart,2);
  const summary=diagnosticSummary(first);
  assert.match(summary,/names: \["L","Hα",""\]/); assert.match(summary,/focusOffsets: \[-12,0,17\]/);
  const last=await setup.read(wheelOutput.id,2,32); assert.equal(last.nextStart,null); assert.match(diagnosticSummary(last),/position: -1/);
  assert.equal((await setup.read(wheelOutput.id,3,1)).diagnostics.properties.length,0);
}
for (const [index,value] of [
  [0,[]],[0,[1]],[0,[['L']]],[0,Array(1025).fill('L')],
  [1,[]],[1,[1,2]],[1,[0,1.5]],[1,[0,2147483648]],[1,[0,-2147483649]],
  [1,[0,'1']],[1,[0,[1]]],[1,Array(1025).fill(0)], [2,-2],[2,1024],[2,0.5]
]) {
  let revoked=false;
  const setup=new OutputDiagnostics(async()=>{
    const reply=wheelReply(index,1); reply.diagnostics.properties[0].sample.reading.value.value=value; return reply;
  },()=>revoked=true);
  setup.load(description,wheelSaved); await assert.rejects(setup.read(wheelOutput.id,index,1));
  assert.equal(revoked,true); assert.equal(setup.observation,null);
}
for (const fault of ['property','source','generation','sequence','type','extra','age']) {
  let revoked=false;
  const setup=new OutputDiagnostics(async()=>{
    const reply=wheelReply(0,1), item=reply.diagnostics.properties[0], reading=item.sample.reading;
    if(fault==='property') item.property='position';
    if(fault==='source') reading.source='88888888-8888-4888-8888-888888888888';
    if(fault==='generation') reading.generation='88888888-8888-4888-8888-888888888888';
    if(fault==='sequence') reading.sequence=1;
    if(fault==='type') reading.value={type:'integer',value:0};
    if(fault==='extra') reading.authorization='PRIVATE_FORBIDDEN_REPLY';
    if(fault==='age') reading.ageSeconds=-1;
    return reply;
  },()=>revoked=true);
  setup.load(description,wheelSaved); await assert.rejects(setup.read(wheelOutput.id,0,1));
  assert.equal(revoked,true); assert.equal(setup.observation,null);
}
console.log('Wheel diagnostic metadata, paging, identity and signed Int32 bounds passed.');

const panelSaved=structuredClone(focuserSaved);
panelSaved.sources[0].backend={kind:'native',device:'ofp2',identity:'SIM-OFP2'};
panelSaved.outputs[0].device.deviceType='covercalibrator';
const panelOutput=panelSaved.outputs[0];
function panelReply(start=0,limit=6) {
  const fields=description.outputDiagnostics.covercalibratorProperties, health=diagHealth(panelSaved.sources[0].id);
  health.transportConnected=true; health.leaseCount=1;
  const values={brightness:0,maxBrightness:4096,coverState:4,calibratorState:3,coverMoving:false,calibratorChanging:false};
  const properties=fields.slice(start,start+limit).map(field=>({property:field.property,sample:{state:'available',reading:{
    value:{type:field.valueType,value:values[field.property]},ageSeconds:5,source:health.source,
    generation:health.generation,sequence:0,revision:panelSaved.revision}}}));
  const end=Math.min(start+limit,fields.length);
  return {purpose:'cachedDiagnostics',output:panelOutput.id,configurationRevision:panelSaved.revision,observedSeconds:6,
    deviceType:'covercalibrator',simulated:true,start,limit,total:fields.length,nextStart:end<fields.length?end:null,
    diagnostics:{kind:'covercalibrator',health,properties}};
}
{
  const setup=new OutputDiagnostics(async command=>panelReply(command.start,command.limit),()=>assert.fail('Review revoked'));
  setup.load(description,panelSaved);
  assert.equal((await setup.read(panelOutput.id,0,3)).nextStart,3);
  const last=await setup.read(panelOutput.id,3,32);
  assert.equal(last.nextStart,null); assert.match(diagnosticSummary(last),/calibratorState: 3/);
  assert.match(diagnosticSummary(last),/coverMoving: false/);
  assert.equal((await setup.read(panelOutput.id,6,1)).diagnostics.properties.length,0);
}
for (const [index,value] of [[0,-1],[0,2147483648],[0,0.5],[1,0],[1,2147483648],[2,6],[3,-1],[4,0],[5,'false']]) {
  let revoked=false;
  const setup=new OutputDiagnostics(async()=>{
    const reply=panelReply(index,1); reply.diagnostics.properties[0].sample.reading.value.value=value; return reply;
  },()=>revoked=true);
  setup.load(description,panelSaved); await assert.rejects(setup.read(panelOutput.id,index,1));
  assert.equal(revoked,true); assert.equal(setup.observation,null);
}
{
  const setup=new OutputDiagnostics(async()=>{
    const reply=panelReply(4,1); reply.diagnostics.properties[0].sample={state:'unavailable',error:{kind:'unavailable',message:'Unknown cover completion',upstreamCode:null}}; return reply;
  },()=>assert.fail('Review revoked'));
  setup.load(description,panelSaved);
  assert.match(diagnosticSummary(await setup.read(panelOutput.id,4,1)),/Unknown cover completion/);
}
console.log('Panel diagnostic states, completion availability, paging and Int32 bounds passed.');
