import { configurationContract } from './hub-config.mjs';
import { renderConfiguration, previewValue } from './hub-form.mjs';
const $ = id => document.getElementById(id);
let base, draft, reader, description, reviewed, dirty = false, busy = false, uncertain = false;
function status(message, error = false) { $('status').textContent = message; $('status').classList.toggle('error', error); }
function errors(fields = []) { $('errors').replaceChildren(...fields.map(field => { const item = document.createElement('li'); item.textContent = `${field.path}: ${field.message}`; return item; })); }
function changed() { dirty = true; reviewed = undefined; $('apply').disabled = true; $('preview').hidden = true; status('Unsaved changes. Review before applying.'); }
function controls() { $('editor-fields').disabled = busy || !reader || uncertain; $('validate').disabled = busy || !reader || uncertain; $('apply').disabled = busy || !reviewed || uncertain; $('reload').disabled = busy; }
async function rpc(command) {
  const response = await fetch('/setup/api/hub', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(command),cache:'no-store'});
  const value = await response.json();
  if (!response.ok || value.error) { const error = new Error(value.error?.message ?? 'Hub request failed'); error.detail = value.error; throw error; }
  return value.result;
}
async function action(work, mutation = false) {
  if (busy) return;
  busy = true; controls(); errors();
  try { await work(); }
  catch (error) {
    errors(error.detail?.fields);
    if (mutation && (!error.detail || ['uncertain','disconnected'].includes(error.detail.code))) {
      uncertain = true; reviewed = undefined;
      status('Apply outcome is unknown. Reload and inspect the saved revision and host status before making another change. Do not repeat Apply.', true);
    } else status(error.message, true);
  } finally { busy = false; controls(); }
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
  // Read status separately so applied-but-blocked outcomes remain visible.
  description = await rpc({op:'describeConfig'});
  const saved = await rpc({op:'getConfig'});
  const host = await rpc({op:'hostStatus'});
  reader = configurationContract(description); base = saved; draft = structuredClone(saved); reviewed = undefined;
  uncertain = host.phase !== 'ready'; dirty = false;
  $('host-state').textContent = `Host: ${host.phase}${host.persistenceWarning ? ` · ${host.persistenceWarning}` : ''}`;
  $('revision').textContent = `Saved revision: ${base.revision}`;
  renderConfiguration($('configuration'),reader,draft,base,changed); renderSources(); $('preview').hidden = true;
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
action(load);
