import { configurationContract } from './hub-config.mjs';
import { renderConfiguration, previewValue } from './hub-form.mjs';
import { CredentialSetup } from './hub-credentials.mjs';
import { SimulationSetup, parseSimulationArray } from './hub-simulation.mjs';
import { OutputDiagnostics, diagnosticSummary } from './hub-diagnostics.mjs';
import { AlpacaDiscovery, AlpacaNetworkDiscovery, catalogSummary } from './hub-discovery.mjs';
const $ = id => document.getElementById(id);
let base, draft, reader, description, reviewed, dirty = false, busy = false, uncertain = false;
let publicHostStatus;
function status(message, error = false) { $('status').textContent = message; $('status').classList.toggle('error', error); }
function errors(fields = []) { $('errors').replaceChildren(...fields.map(field => { const item = document.createElement('li'); item.textContent = `${field.path}: ${field.message}`; return item; })); }
function revokeReview() { reviewed = undefined; $('apply').disabled = true; $('preview').hidden = true; }
function changed() { dirty = true; revokeReview(); status('Unsaved changes. Review before applying.'); }
const credentials = new CredentialSetup(rpc, reference => { $('credential-reference').value = reference; }, revokeReview);
const outputDiagnostics = new OutputDiagnostics(rpc,revokeReview);
const discovery = new AlpacaDiscovery(rpc);
const networkDiscovery = new AlpacaNetworkDiscovery(rpc);
function controls() {
  $('editor-fields').disabled = busy || !reader || uncertain; $('validate').disabled = busy || !reader || uncertain; $('apply').disabled = busy || !reviewed || uncertain; $('reload').disabled = busy;
  for (const button of $('sources').querySelectorAll('button')) button.disabled = busy || uncertain;
  for (const fields of $('sources').querySelectorAll('.simulation-fields')) fields.disabled = busy || uncertain;
  for (const fields of $('outputs').querySelectorAll('fieldset')) fields.disabled = busy || uncertain;
  $('export-diagnostics').disabled = busy || !outputDiagnostics.observation;
  const fields = $('credential-fields'); if (fields) fields.disabled = busy || uncertain || credentials.uncertain;
  const create = $('credential-create'); if (create) create.disabled = busy || uncertain || credentials.description?.clientChosenReferences !== true;
  const catalog = $('discovery-fields'); if (catalog) catalog.disabled = busy || uncertain || discovery.uncertain || networkDiscovery.uncertain;
}
async function rpc(command, path = '/setup/api/hub', deadlineSeconds = 40) {
  const abort = new AbortController(); const timer = setTimeout(() => abort.abort(),deadlineSeconds * 1000);
  try {
    const response = await fetch(path, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(command),cache:'no-store',signal:abort.signal});
    const value = await response.json();
    if (!response.ok || value.error) { const error = new Error(value.error?.message ?? 'Hub request failed'); error.detail = value.error; throw error; }
    return value.result;
  } finally { clearTimeout(timer); }
}
async function action(work, mutation = false) {
  if (busy) return;
  busy = true; controls(); errors();
  try { await work(); }
  catch (error) {
    errors(error.detail?.fields);
    if (credentials.uncertain || error.uncertain || mutation && (!error.detail || ['uncertain','disconnected','unavailable','timeout','revisionConflict'].includes(error.detail.code))) {
      uncertain = true; reviewed = undefined;
      status('The outcome is unknown. Reload and inspect the saved revision, host status and affected source; read a retained credential reference before another credential change. Do not repeat the write.', true);
    } else status(error.message, true);
  } finally { busy = false; controls(); }
}
function renderCredentials() {
  const root = $('credential-controls'); root.replaceChildren();
  const storage = credentials.description;
  const text = value => { const p = document.createElement('p'); p.className = 'hint'; p.textContent = value; root.append(p); };
  if (!storage) { text('This host has no credential storage provider.'); return; }
  text(`${storage.protectionDescription}. ${storage.rotation}`);
  const form = document.createElement('form'); form.autocomplete = 'off'; root.append(form);
  const fields = document.createElement('fieldset'); fields.id = 'credential-fields'; form.append(fields);
  const field = (schema,id,type) => {
    const label = document.createElement('label'); label.textContent = schema.label;
    const input = document.createElement('input'); input.id = id; input.type = type; input.autocomplete = 'off'; input.spellcheck = false; input.setAttribute('autocapitalize','off'); input.title = schema.description; label.append(input); fields.append(label);
    const description = document.createElement('p'); description.className = 'hint'; description.textContent = schema.description; fields.append(description); return input;
  };
  const secret = field(storage.input.authorization,'credential-authorization','password'); secret.maxLength = storage.input.authorization.maxLength;
  const button = (id,label,work,mutation) => { const b = document.createElement('button'); b.id = id; b.type = 'button'; b.textContent = label; b.onclick = () => action(work,mutation); fields.append(b); return b; };
  const create = async () => {
    const authorization = secret.value; secret.value = '';
    await credentials.create(authorization);
    status('Credential saved. Copy its reference into the source settings, then review and apply.');
  };
  button('credential-create','Save new credential',create,true);
  const reference = field(storage.reference,'credential-reference','text'); reference.value = credentials.reference;
  reference.oninput = () => credentials.setReference(reference.value);
  button('credential-status','Read credential status',async () => {
    const result = await credentials.status(); status(result.present ? 'Credential is present. Its value is never returned.' : 'Credential is absent. Check the reference before creating a new credential.');
  },false);
  button('credential-delete','Remove unused credential',async () => {
    const result = await credentials.remove(); status(`${result.removed ? 'Unused credential removed.' : 'Credential was already absent.'}${result.persistenceWarning ? ' ' + result.persistenceWarning : ''}`);
  },true);
  form.onsubmit = event => { event.preventDefault(); if (!busy && !uncertain && !credentials.uncertain && storage.clientChosenReferences === true) action(create,true); };
  text('After a lost reply, Reload and read the retained reference before another change. Removing a credential is refused while the saved configuration uses it. Retain the reference separately before leaving this page.');
}
function renderDiscovery() {
  const root=$('discovery-controls'); root.replaceChildren();
  const d=discovery.description, fields=document.createElement('fieldset'); fields.id='discovery-fields'; root.append(fields);
  const network=networkDiscovery.description;
  const heading=document.createElement('h3'); heading.textContent=network.label; fields.append(heading);
  const networkNote=document.createElement('p'); networkNote.textContent=network.description; fields.append(networkNote);
  const search=document.createElement('button'); search.type='button'; search.textContent='Find Alpaca servers'; fields.append(search);
  const candidates=document.createElement('select'); candidates.id='network-discovery-selection'; candidates.setAttribute('aria-label','Server candidate'); fields.append(candidates);
  const choose=document.createElement('button'); choose.type='button'; choose.textContent='Use selected server address'; choose.disabled=true; fields.append(choose);
  const searchStatus=document.createElement('p'); fields.append(searchStatus);
  candidates.onchange=()=>{choose.disabled=candidates.selectedIndex<0 || candidates.selectedOptions[0].disabled;};
  const field=(key)=>{
    const schema=d.parameters[key], label=document.createElement('label'), input=document.createElement('input');
    label.textContent=schema.label; input.type='text'; input.autocomplete='off'; input.id=`discovery-${key}`;
    if (schema.maxLength) input.maxLength=schema.maxLength;
    if (schema.type==='integer') { input.type='number'; input.step='1'; input.min=schema.minimum; input.max=schema.maximum; }
    input.title=schema.description; label.append(input); fields.append(label); return input;
  };
  const url=field('baseUrl'), scope=field('scopeId'), credential=field('credentialReference');
  const note=document.createElement('p'); note.className='hint'; note.textContent=`One query, up to ${d.timeoutSeconds} seconds and ${d.maximumDevices} devices. Include any reverse-proxy prefix. Credentials are protected references from the Credentials section.`; fields.append(note);
  const button=document.createElement('button'); button.type='button'; button.textContent='Read Alpaca device catalog'; fields.append(button);
  const selection=document.createElement('select'); selection.id='discovery-selection'; selection.setAttribute('aria-label','Catalog device'); fields.append(selection);
  const add=document.createElement('button'); add.type='button'; add.textContent='Add selected source to draft'; add.disabled=true; fields.append(add);
  selection.onchange=()=>{add.disabled=selection.selectedIndex<0 || selection.selectedOptions[0].disabled;};
  choose.onclick=()=>{
    if (candidates.selectedIndex<0 || candidates.selectedOptions[0].disabled) return;
    const server=networkDiscovery.result.servers[Number(candidates.value)];
    url.value=server.baseUrl; scope.value=server.scopeId===0?'':String(server.scopeId); credential.value='';
    selection.replaceChildren(); add.disabled=true; $('discovery-result').textContent='';
    status('Server address selected. Choose a credential reference if needed, then read its catalog.');
  };
  search.onclick=()=>action(async()=>{
    candidates.replaceChildren(); choose.disabled=true; searchStatus.textContent='';
    const found=await networkDiscovery.search();
    for (const [index,server] of found.servers.entries()) {
      const option=document.createElement('option'); option.value=String(index);
      option.textContent=server.baseUrl+(server.scopeId===0?'':` (interface ${server.scopeId})`);
      candidates.append(option);
    }
    candidates.selectedIndex=-1;
    searchStatus.textContent=`${found.servers.length} server candidates. ${found.incomplete?'Search was incomplete; an interface failed or a limit was reached. ':''}No catalog was read and no equipment connection was opened.`;
  });
  add.onclick=()=>action(async()=>{
    const id=discovery.adopt(reader,draft,Number(selection.value));
    changed(); renderConfiguration($('configuration'),reader,draft,base,changed);
    status(`Added pinned source ${id} to the draft. Review its settings and apply before creating output connections.`);
  });
  button.onclick=()=>action(async()=>{
    $('discovery-result').textContent=''; selection.replaceChildren(); add.disabled=true;
    const catalog=await discovery.query(url.value,credential.value.trim()?credential.value:null,scope.value===''?null:Number(scope.value));
    $('discovery-result').textContent=catalogSummary(catalog);
    catalog.devices.forEach((device,index)=>{
      const option=document.createElement('option'); option.value=String(index); option.textContent=`${device.name} — ${device.reportedDeviceType} ${device.number}`;
      option.disabled=device.supportedDeviceType===null; selection.append(option);
    });
    selection.selectedIndex=-1;
    status(`Read ${catalog.devices.length} catalog entries. No equipment connection was opened; configuration is unchanged.`);
  });
  $('discovery-result').textContent='';
}
function renderSources() {
  $('sources').replaceChildren(...base.sources.map(source => {
    const box = document.createElement('article');
    const title = document.createElement('strong'); title.textContent = source.label; box.append(title);
    const inspection = document.createElement('form'); box.append(inspection);
    inspection.onsubmit = event => event.preventDefault();
    const parameters = {};
    for (const [key, field] of Object.entries(description.capabilityInspection.parameters)) {
      const label = document.createElement('label'); label.textContent = field.label;
      const input = document.createElement('input'); input.type = 'number'; input.step = '1'; input.min = field.minimum; input.max = field.maximum; input.value = field.default; input.required = true;
      input.title = field.description; label.append(input); inspection.append(label); parameters[key] = input;
    }
    for (const [text, op] of [['Status','sourceStatus'],['Inspect','inspectSource']]) {
      const button = document.createElement('button'); button.type = 'button'; button.textContent = text;
      button.onclick = () => action(async () => {
        if (op === 'inspectSource' && !inspection.reportValidity()) return;
        const result = await rpc({op,source:source.id,...(op === 'inspectSource' ? Object.fromEntries(Object.entries(parameters).map(([key,input]) => [key,input.valueAsNumber])) : {})});
        $('source-result').hidden = false; $('source-result').textContent = JSON.stringify(result,null,2); status(`${text}: ${source.label}`);
      }); box.append(button);
    }
    if (description.simulationControl?.sourceKinds?.includes(source.backend.kind)) renderSimulation(box,source);
    return box;
  }));
}
function renderOutputs() {
  $('output-result').hidden=true; $('output-summary').textContent='';
  $('outputs').replaceChildren(...base.outputs.map(output=>{
    const box=document.createElement('article'), title=document.createElement('strong'); title.textContent=output.label; box.append(title);
    const fields=document.createElement('fieldset'); box.append(fields); const parameters={}; let next;
    for(const [key,field] of Object.entries(description.outputDiagnostics.parameters)) {
      const label=document.createElement('label'); label.textContent=field.label; const input=document.createElement('input'); input.type='number'; input.step='1'; input.min=field.minimum; input.max=field.maximum; input.value=field.default; input.title=field.description; input.dataset.diagnosticParameter=key;
      input.oninput=()=>{next=undefined; nextButton.disabled=true;}; label.append(input); fields.append(label); parameters[key]=input;
    }
    const read=document.createElement('button'), nextButton=document.createElement('button'); read.type=nextButton.type='button'; read.textContent='Read cached output health'; nextButton.textContent='Read next output items'; nextButton.disabled=true;
    async function observe(start) {
      $('output-result').hidden=true; $('output-summary').textContent=''; next=undefined; nextButton.disabled=true;
      const result=await outputDiagnostics.read(output.id,start,parameters.limit.valueAsNumber);
      $('output-result').hidden=false; $('output-result').textContent=JSON.stringify(result,null,2); $('output-summary').textContent=diagnosticSummary(result);
      parameters.start.value=result.start; next=result.nextStart; nextButton.disabled=next===null || next===undefined;
      status(`Cached output: ${output.label}. No equipment connection or safety confirmation was started.`);
    }
    read.onclick=()=>action(()=>observe(parameters.start.valueAsNumber)); nextButton.onclick=()=>action(()=>observe(next)); fields.append(read,nextButton); return box;
  }));
}
function renderSimulation(box,source) {
  const setup = new SimulationSetup(rpc,revokeReview); setup.load(description.simulationControl,source,base.revision);
  const group = document.createElement('details'); const title = document.createElement('summary'); title.textContent = 'Simulation controls'; group.append(title); box.append(group);
  const note = document.createElement('p'); note.className = 'hint'; note.textContent = `${description.simulationControl.persistence} ${description.simulationControl.uncertainWrites} Select only the fields to change. Read current state to reset this form; initial values are defaults for a new runtime.`; group.append(note);
  const fields = document.createElement('fieldset'); fields.className = 'simulation-fields'; group.append(fields);
  const inputs = setup.controls.map(control => {
    const row = document.createElement('div'); const label = document.createElement('label'); const include = document.createElement('input'); include.type = 'checkbox'; label.append(include,`Change ${control.label}`); row.append(label);
    const hint = document.createElement('p'); hint.className = 'hint'; hint.textContent = control.description; row.append(hint);
    const bounds = [['minimum','Minimum'],['exclusiveMinimum','Greater than'],['maximum','Maximum'],['step','Step'],['minItems','Minimum items'],['maxItems','Maximum items'],['maxUtf8Bytes','Maximum UTF-8 bytes']].filter(([key])=>control[key]!==undefined).map(([key,label])=>`${label} ${control[key]}`);
    if (bounds.length) { const note = document.createElement('p'); note.className='hint'; note.textContent=bounds.join(' · '); row.append(note); }
    const input = document.createElement(control.type === 'string' ? 'select' : 'input'); input.dataset.simulationPath = control.path.join('/');
    if (control.type === 'string') for (const option of control.enum) { const item = document.createElement('option'); item.value = option; item.textContent = option; input.append(item); }
    else {
      input.type = control.type === 'boolean' ? 'checkbox' : ['strings','integers'].includes(control.type) ? 'text' : 'number';
      if (control.type === 'number' || control.type === 'integer') { input.step = control.type === 'integer' ? '1' : 'any'; if (control.minimum !== undefined) input.min = control.minimum; if (control.maximum !== undefined) input.max = control.maximum; }
    }
    input.setAttribute('aria-label',control.label); row.append(input);
    let absent;
    if (control.nullable) { const label = document.createElement('label'); absent = document.createElement('input'); absent.type = 'checkbox'; label.append(absent,'Sensor absent'); row.append(label); }
    function enabled() { input.disabled = !include.checked || absent?.checked === true; if (absent) absent.disabled = !include.checked; }
    include.onchange = enabled; if (absent) absent.onchange = enabled;
    function set(value) { include.checked = false; if (absent) absent.checked = value === null; if (control.type === 'boolean') input.checked = value === true; else input.value = ['strings','integers'].includes(control.type) ? JSON.stringify(value) : value ?? ''; enabled(); }
    set(control.default); fields.append(row);
    return {control,include,input,absent,set};
  });
  const show = value => { for (const item of inputs) { let field = value; for (const key of item.control.path) field = field[key]; item.set(field); } $('source-result').hidden = false; $('source-result').textContent = JSON.stringify(value,null,2); };
  const read = document.createElement('button'); read.type = 'button'; read.textContent = 'Read current simulation and reset form';
  read.onclick = () => action(async () => { show(await setup.read()); status(`Current simulation: ${source.label}. No equipment connection was opened.`); }); fields.append(read);
  const apply = document.createElement('button'); apply.type = 'button'; apply.textContent = 'Apply selected simulation changes';
  apply.onclick = () => action(async () => {
    const selected = inputs.filter(i => i.include.checked).map(i => ({path:i.control.path,value:i.absent?.checked ? null : i.control.type === 'boolean' ? i.input.checked : ['number','integer'].includes(i.control.type) ? i.input.valueAsNumber : ['strings','integers'].includes(i.control.type) ? parseSimulationArray(i.control,i.input.value) : i.input.value}));
    show(await setup.update(selected)); status(`Simulation updated: ${source.label}. Configuration is unchanged; normal safety polling and confirmation apply.`);
  },true); fields.append(apply);
}
async function load() {
  uncertain = true; revokeReview();
  outputDiagnostics.observation=null; publicHostStatus=undefined;
  const password = $('credential-authorization'); if (password) password.value = '';
  await rpc({}, '/setup/api/hub/reload');
  // Read status separately so applied-but-blocked outcomes remain visible.
  description = await rpc({op:'describeConfig'});
  const saved = await rpc({op:'getConfig'});
  const host = await rpc({op:'hostStatus'});
  reader = configurationContract(description); base = saved; draft = structuredClone(saved); reviewed = undefined;
  if (host.configurationRevision !== saved.revision) throw new Error('Saved configuration changed during reload; reload again');
  credentials.load(description.credentialStorage);
  outputDiagnostics.load(description,saved); publicHostStatus=structuredClone(host);
  discovery.load(description,saved);
  networkDiscovery.load(description,saved);
  uncertain = host.phase !== 'ready'; dirty = false;
  $('host-state').textContent = `Host: ${host.phase}${host.persistenceWarning ? ` · ${host.persistenceWarning}` : ''}`;
  $('revision').textContent = `Saved revision: ${base.revision}`;
  renderConfiguration($('configuration'),reader,draft,base,changed); renderSources(); renderOutputs(); renderCredentials(); renderDiscovery(); $('preview').hidden = true;
  status(uncertain ? 'Host is not ready. Inspect its status before applying another change.' : 'Configuration loaded. Source connections are shared across frontends.', uncertain);
}
$('reload').onclick = () => {
  if (dirty && !confirm('Discard your unsaved changes and reload the saved configuration?')) return;
  action(load);
};
$('editor').onsubmit = event => event.preventDefault();
$('export-diagnostics').onclick=()=>action(async()=>{
  const snapshot={format:'regainHubSetupDiagnostics',version:1,exportedAtUtc:new Date().toISOString(),instanceId:base.instanceId,editorState:uncertain?'Uncertain':reviewed?'Reviewed':'Editing',savedRevision:base.revision,savedHostStatus:publicHostStatus,observation:null,outputObservation:outputDiagnostics.observation};
  const url=URL.createObjectURL(new Blob([JSON.stringify(snapshot,null,2)],{type:'application/json'}));
  const link=document.createElement('a'); link.href=url; link.download='regain-hub-diagnostics.json'; link.click(); setTimeout(()=>URL.revokeObjectURL(url),1000);
  status('Exported observed host/output status with timestamps and revision. Editable configuration and credentials are excluded.');
});
$('editor').addEventListener('invalid', event => {
  for (let node = event.target.parentElement; node; node = node.parentElement) if (node.tagName === 'DETAILS') node.open = true;
}, true);
$('validate').onclick = () => {
  if (!$('editor').reportValidity()) return;
  action(async () => {
    const candidate = structuredClone(draft);
    const result = await rpc({op:'validateConfig',candidate}); errors(result.errors);
    $('preview').hidden = false; $('preview-json').textContent = JSON.stringify(previewValue(reader, reader.root, candidate), null, 2);
    reviewed = result.valid ? candidate : undefined;
    status(result.valid ? 'Configuration is valid. Review it, disconnect output clients, then apply.' : 'Correct the validation errors before applying.', !result.valid);
  });
};
$('apply').onclick = () => action(async () => {
  const candidate = reviewed; reviewed = undefined;
  const result = await rpc({op:'applyConfig',expectedRevision:base.revision,candidate});
  await load();
  status(result.ready ? `Saved revision ${result.configurationRevision}.${result.persistenceWarning ? ` ${result.persistenceWarning}` : ''}` : 'Configuration was saved, but the host is blocked by source cleanup. Inspect host status before connecting equipment.', !result.ready);
}, true);
window.addEventListener('beforeunload', event => { if (dirty) { event.preventDefault(); event.returnValue = ''; } });
window.addEventListener('pagehide', () => { const password = $('credential-authorization'); if (password) password.value = ''; });
action(load);
