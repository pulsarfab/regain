import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { configurationContract } from '../crates/regain-alpaca/web/hub-config.mjs';
import { initialValue, newIdentity, previewValue } from '../crates/regain-alpaca/web/hub-form.mjs';
import { CredentialSetup, credentialContract } from '../crates/regain-alpaca/web/hub-credentials.mjs';
import { SimulationSetup, simulationControls, validateSimulationValue } from '../crates/regain-alpaca/web/hub-simulation.mjs';
import { OutputDiagnostics, diagnosticSummary, validateDiagnosticSchema } from '../crates/regain-alpaca/web/hub-diagnostics.mjs';

const description = JSON.parse(readFileSync(new URL('../contracts/hub-config.json', import.meta.url), 'utf8'));
const reader = configurationContract(description);
const source = reader.root.$defs.SourceBackend;
const variants = reader.variants(source, ['alpacaSources']);
assert.equal(variants.find(v => v.kind === 'alpaca').enabled, true);
assert.equal(variants.find(v => v.kind === 'com').enabled, false);
const com = reader.variants(source, ['comSources']).find(v => v.kind === 'com');
assert.equal(com.enabled, true);
assert.deepEqual(reader.choices(com.schema.properties.deviceType).filter(c => c.enabled).map(c => c.value), ['switch','safetymonitor','observingconditions']);
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
    else state[control.path[0]][control.path[1]] = control.default;
  }
  return state;
}
for (const type of ['switch','safetymonitor','observingconditions']) {
  const fields = simulationControls(simDescription,simulatedSource(type));
  assert.equal(fields.length,type==='switch'?5:type==='safetymonitor'?2:15);
  assert.deepEqual(fields.find(f=>f.path[0]==='fault').enum,simDescription.faultsByDeviceType[type]);
}
assert.deepEqual(simulationControls(simDescription,{backend:{kind:'native'}}),[]);
const weatherFields = simulationControls(simDescription,simulatedSource('observingconditions'));
const pressure = weatherFields.find(f=>f.path[1]==='pressure');
assert.throws(()=>validateSimulationValue(pressure,0)); assert.equal(validateSimulationValue(pressure,null),null);
assert.throws(()=>validateSimulationValue(weatherFields.find(f=>f.path[1]==='temperature'),-273.16));
for (const failure of ['lost','wrongRevision','wrongSource','malformed','unavailable']) {
  let requests = 0, review = true; const state = simulationState('switch');
  const source = simulatedSource('switch');
  const setup = new SimulationSetup(async command => {
    if (command.op==='sourceStatus') return {source:source.id,revision:simRevision,simulated:true,simulation:state};
    requests++; assert.equal(command.expectedRevision,simRevision); assert.deepEqual(command.update,{switchValues:{1:42}});
    state.switchValues[1]=42;
    if (failure==='lost') throw new Error('Lost reply');
    if (failure==='unavailable') { const error = new Error('Unavailable'); error.detail={code:'unavailable'}; throw error; }
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
const diagHealth=source=>({source,revision:diagSaved.revision,generation:diagGeneration,sequence:0,transportConnected:false,writeUncertain:false,leaseCount:0,error:null});
const diagUnavailable={state:'unavailable',error:{kind:'disconnected',message:'Source is disconnected',upstreamCode:null}};
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
for(const fault of ['lost','revision','output','cursor','channel','secretRoot','secretHealth','type','emptyGeneration','missingError']) {
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
