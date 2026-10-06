import { configurationContract } from './hub-config.mjs';
import { renderConfiguration, previewValue } from './hub-form.mjs';
import { CredentialSetup } from './hub-credentials.mjs';
const $ = id => document.getElementById(id);
let base, draft, reader, description, reviewed, dirty = false, busy = false, uncertain = false;
function status(message, error = false) { $('status').textContent = message; $('status').classList.toggle('error', error); }
function errors(fields = []) { $('errors').replaceChildren(...fields.map(field => { const item = document.createElement('li'); item.textContent = `${field.path}: ${field.message}`; return item; })); }
function revokeReview() { reviewed = undefined; $('apply').disabled = true; $('preview').hidden = true; }
function changed() { dirty = true; revokeReview(); status('Unsaved changes. Review before applying.'); }
const credentials = new CredentialSetup(rpc, reference => { $('credential-reference').value = reference; }, revokeReview);
function controls() {
  $('editor-fields').disabled = busy || !reader || uncertain; $('validate').disabled = busy || !reader || uncertain; $('apply').disabled = busy || !reviewed || uncertain; $('reload').disabled = busy;
  for (const button of $('sources').querySelectorAll('button')) button.disabled = busy || uncertain;
  const fields = $('credential-fields'); if (fields) fields.disabled = busy || uncertain || credentials.uncertain;
  const create = $('credential-create'); if (create) create.disabled = busy || uncertain || credentials.description?.clientChosenReferences !== true;
}
async function rpc(command, path = '/setup/api/hub') {
  const abort = new AbortController(); const timer = setTimeout(() => abort.abort(),40000);
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
    if (credentials.uncertain || mutation && (!error.detail || ['uncertain','disconnected','unavailable','timeout','revisionConflict'].includes(error.detail.code))) {
      uncertain = true; reviewed = undefined;
      status('The outcome is unknown. Reload and inspect the saved revision and host status; read the retained credential reference before another credential change. Do not repeat the write.', true);
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
    return box;
  }));
}
async function load() {
  uncertain = true; revokeReview();
  const password = $('credential-authorization'); if (password) password.value = '';
  await rpc({}, '/setup/api/hub/reload');
  // Read status separately so applied-but-blocked outcomes remain visible.
  description = await rpc({op:'describeConfig'});
  const saved = await rpc({op:'getConfig'});
  const host = await rpc({op:'hostStatus'});
  reader = configurationContract(description); base = saved; draft = structuredClone(saved); reviewed = undefined;
  if (host.configurationRevision !== saved.revision) throw new Error('Saved configuration changed during reload; reload again');
  credentials.load(description.credentialStorage);
  uncertain = host.phase !== 'ready'; dirty = false;
  $('host-state').textContent = `Host: ${host.phase}${host.persistenceWarning ? ` · ${host.persistenceWarning}` : ''}`;
  $('revision').textContent = `Saved revision: ${base.revision}`;
  renderConfiguration($('configuration'),reader,draft,base,changed); renderSources(); renderCredentials(); $('preview').hidden = true;
  status(uncertain ? 'Host is not ready. Inspect its status before applying another change.' : 'Configuration loaded. Source connections are shared across frontends.', uncertain);
}
$('reload').onclick = () => {
  if (dirty && !confirm('Discard your unsaved changes and reload the saved configuration?')) return;
  action(load);
};
$('editor').onsubmit = event => event.preventDefault();
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
