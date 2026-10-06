import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { configurationContract } from '../crates/regain-alpaca/web/hub-config.mjs';
import { initialValue, newIdentity, previewValue } from '../crates/regain-alpaca/web/hub-form.mjs';
import { CredentialSetup, credentialContract } from '../crates/regain-alpaca/web/hub-credentials.mjs';
import { SimulationSetup, simulationControls, validateSimulationValue } from '../crates/regain-alpaca/web/hub-simulation.mjs';

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
