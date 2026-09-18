'use strict';
const $ = id => document.getElementById(id);
const client = crypto.getRandomValues(new Uint32Array(1))[0];
let connected = false, busy = false;
async function api(url, body) {
  const response = await fetch(url, body === undefined ? {} : {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(body)});
  const value = await response.json(); if (!response.ok) throw Error(value.error || response.statusText); return value;
}
async function command(member, values) {
  const p = new URLSearchParams({ClientID:String(client), ClientTransactionID:'0', ...values});
  const response = await fetch('/api/v1/rotator/0/' + member + (values === undefined ? '?' + p : ''), values === undefined ? {} : {method:'PUT',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:p});
  const v = await response.json(); if (v.ErrorNumber) throw Error(v.ErrorMessage); return v.Value;
}
function controls(anyConnected) {
  $('scan').disabled = busy || anyConnected; $('device').disabled = busy || anyConnected;
  for (const id of ['move','sync','apply']) $(id).disabled = busy || !connected;
  $('halt').disabled = !connected; $('connect').disabled = busy; $('connect').textContent = connected ? 'Disconnect setup' : 'Connect for setup';
}
async function refresh() {
  const state = await api('/setup/api/rotator'); connected = await command('connected');
  $('simulation').hidden = !state.simulation;
  $('connection').textContent = state.connected ? 'Connected' : 'Disconnected'; controls(state.connected);
  if (connected) $('position').textContent = `Sky ${Number(await command('position')).toFixed(2)}° · Mechanical ${Number(await command('mechanicalposition')).toFixed(2)}° · ${await command('ismoving') ? 'Moving' : 'Idle'}`;
  else $('position').textContent = '';
  return state;
}
async function run(action) {
  if (busy) return; busy = true; controls(true); $('status').textContent = '';
  try { await action(); } catch(e) { $('status').textContent = e.message; }
  finally { busy = false; try { await refresh(); } catch(e) { $('status').textContent = e.message; } }
}
$('scan').onclick = () => run(async () => {
  const saved = $('device').value, list = await api('/setup/api/rotator/discover', {});
  const options = list.filter(v => v.identity).map(v => new Option(`${v.identity.model} — ${v.identity.serial}`, v.identity.serial));
  if (saved && !options.some(o => o.value === saved)) options.unshift(new Option('Saved CAA — ' + saved, saved));
  $('device').replaceChildren(new Option('No rotator selected',''), ...options); $('device').value = saved;
});
$('device').onchange = () => run(() => api('/setup/api/rotator', {serial:$('device').value || null}));
$('connect').onclick = () => run(async () => { await command('connected', {Connected:String(!connected)}); connected = !connected; if (connected) $('reverse').checked = await command('reverse'); });
$('halt').onclick = async () => { try { await command('halt', {}); await refresh(); } catch(e) { $('status').textContent=e.message; } };
function angle() { const n = Number($('angle').value); if ($('angle').value === '' || !Number.isFinite(n) || n < 0 || n >= 360) throw Error('Enter an angle from 0 to less than 360°.'); return String(n); }
$('move').onclick = () => run(() => command('movemechanical', {Position:angle()}));
$('sync').onclick = () => run(() => command('sync', {Position:angle()}));
$('apply').onclick = () => run(() => command('reverse', {Reverse:String($('reverse').checked)}));
// Release only this page's client; other Alpaca clients keep their connection.
window.addEventListener('pagehide', () => { if (connected) fetch('/api/v1/rotator/0/connected', {method:'PUT',keepalive:true,headers:{'Content-Type':'application/x-www-form-urlencoded'},body:new URLSearchParams({ClientID:String(client),Connected:'false'})}).catch(()=>{}); });
(async () => { try { const state = await refresh(); const serial=state.profile.serial; if (serial) { $('device').add(new Option('CAA — ' + serial, serial)); $('device').value=serial; } setInterval(() => { if (!busy) refresh().catch(e => $('status').textContent=e.message); },2000); } catch(e) { $('status').textContent=e.message; } })();
