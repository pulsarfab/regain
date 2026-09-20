'use strict';
const $ = id => document.getElementById(id);
const setup = '/setup/api/flatpanel', api = '/api/v1/covercalibrator/0/';
const client = crypto.getRandomValues(new Uint32Array(1))[0] || 1;
let profile, connected = false, busy = false, faulted = false, moving = false;
let refreshing = null;
async function json(url, body) {
  const response = await fetch(url, body === undefined ? {} : {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(body)});
  const value = await response.json();
  if (!response.ok || value.error) throw Error(value.error || response.statusText);
  return value;
}
async function alpaca(member, body) {
  const params = new URLSearchParams({...body, ClientID:client});
  const response = await fetch(api + member + (body === undefined ? '?' + params : ''), body === undefined ? {} : {method:'PUT', headers:{'Content-Type':'application/x-www-form-urlencoded'}, body:params});
  const value = await response.json();
  if (!response.ok || value.ErrorNumber) throw Error(value.ErrorMessage || response.statusText);
  return value.Value;
}
function controls() {
  $('scan').disabled = $('device').disabled = connected || busy;
  $('connect').disabled = busy || !profile?.serial;
  for (const id of ['open','close','halt','on','off']) $(id).disabled = !connected || busy || faulted || (moving && ['open','close'].includes(id));
  $('connect').textContent = connected ? 'Disconnect setup' : 'Connect for setup';
}
function showError(e) { $('status').textContent = e.message; $('status').className = 'error'; }
async function run(action) {
  if (busy) return;
  busy = true; controls();
  try { if (refreshing) await refreshing; await action(); $('status').textContent = 'Ready.'; $('status').className = ''; }
  catch (e) { showError(e); }
  finally { busy = false; controls(); }
}
function options(items = []) {
  $('device').replaceChildren(new Option('No device selected',''));
  for (const d of items) $('device').add(new Option(`${d.model} — ${d.port} — ${d.serial}`, d.serial));
  if (profile.serial && !Array.from($('device').options).some(o => o.value === profile.serial)) $('device').add(new Option('Saved OFP2 — ' + profile.serial, profile.serial));
  $('device').value = profile.serial || '';
}
async function refresh() {
  if (!connected || faulted) return;
  let state;
  try { state = JSON.parse(await alpaca('action',{Action:'Regain.Status', Parameters:''})); }
  catch (e) { faulted = true; $('cover-state').textContent = 'Error'; $('light-state').textContent = 'State unavailable — check the panel and reconnect.'; throw e; }
  moving = state.cover === 'moving';
  $('cover-state').textContent = ({open:'Open',closed:'Closed',moving:'Moving…',unknown:'Stopped between endpoints'})[state.cover] || 'Unknown';
  $('light-state').textContent = state.calibrator_on ? `On · brightness ${state.brightness} / ${state.max_brightness}` : 'Off';
}
$('scan').onclick = () => run(async () => { const devices = await json(setup + '/discover',{}); options(devices); if (!devices.length) throw Error('No available OFP2 found. Check USB and power, and close other panel applications.'); });
$('device').onchange = () => run(async () => {
  const updated = {...profile, serial:$('device').value || null};
  try { await json(setup,updated); profile = updated; } finally { $('device').value = profile.serial || ''; }
});
$('connect').onclick = () => run(async () => {
  await alpaca('connected',{Connected:!connected}); connected = !connected; faulted = false; moving = false;
  $('connection').textContent = connected ? 'Connected for setup · USB serial' : 'Disconnected';
  if (connected) {
    const info = JSON.parse(await alpaca('action',{Action:'Regain.Identity',Parameters:''}));
    $('identity').textContent = `${info.port} · ${info.firmware}`;
    await refresh();
  } else { $('cover-state').textContent = $('light-state').textContent = 'Disconnected'; }
});
for (const [id, member] of [['open','opencover'],['close','closecover'],['halt','haltcover'],['off','calibratoroff']]) $(id).onclick = () => run(async () => { await alpaca(member,{}); await refresh(); });
$('on').onclick = () => run(async () => {
  const brightness = Number($('brightness').value);
  if ($('brightness').value === '' || !Number.isInteger(brightness) || brightness < 0 || brightness > 4096) throw Error('Enter a whole brightness from 0 to 4096.');
  await alpaca('calibratoron',{Brightness:brightness}); await refresh();
});
setInterval(() => {
  if (connected && !busy && !faulted && !refreshing) {
    refreshing = refresh().catch(showError).finally(() => { refreshing = null; controls(); });
  }
},750);
window.addEventListener('pagehide',() => { if (connected) fetch(api+'connected',{method:'PUT',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:new URLSearchParams({ClientID:client,Connected:false}),keepalive:true}).catch(()=>{}); });
run(async () => { const state = await json(setup); profile = state.profile; $('simulation').hidden = !state.simulation; options(); $('connection').textContent = state.connected ? 'Panel is in use by another Alpaca client' : 'Disconnected'; });
