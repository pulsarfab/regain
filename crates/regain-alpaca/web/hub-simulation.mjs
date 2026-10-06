const fail = message => { const error = new Error(message); error.detail = {code:'invalidValue',message}; throw error; };
const protocol = () => { throw new Error('Invalid simulation setup response'); };
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const members = (value, keys) => { if (!object(value) || Object.keys(value).some(key => !keys.includes(key)) || keys.some(key => !(key in value))) protocol(); };
export function simulationControls(description, source) {
  if (!description?.sourceKinds?.includes(source.backend.kind)) return [];
  const controls = description.controlsByDeviceType?.[source.backend.deviceType];
  if (description.revisionCheckedUpdates !== true || !Array.isArray(controls) || controls.length < 1 || controls.length > 32 || !Number.isInteger(description.deadlineSeconds) || description.deadlineSeconds < 1 || description.deadlineSeconds > 300) protocol();
  const paths = new Set();
  for (const control of controls) {
    if (!Array.isArray(control.path) || control.path.length < 1 || control.path.length > 2 || control.path.some(p => typeof p !== 'string' || !/^[a-zA-Z0-9]+$/.test(p) || p === 'constructor' || p === 'prototype') || typeof control.label !== 'string' || typeof control.description !== 'string') protocol();
    const key = control.path.join('/'); if (paths.has(key)) protocol(); paths.add(key);
    validateSimulationValue(control,control.default);
  }
  return structuredClone(controls);
}
export function validateSimulationValue(control, value) {
  if (value === null && control.nullable === true) return value;
  let valid;
  switch (control.type) {
    case 'boolean': valid = typeof value === 'boolean'; break;
    case 'string': valid = typeof value === 'string' && control.enum.includes(value); break;
    case 'number': {
      const bounds = n => Number.isFinite(n) && (control.minimum === undefined || n >= control.minimum) && (control.maximum === undefined || n <= control.maximum) && (control.exclusiveMinimum === undefined || n > control.exclusiveMinimum) && (control.exclusiveMaximum === undefined || n < control.exclusiveMaximum);
      valid = typeof value === 'number' && bounds(value) && (control.precision === undefined || control.precision === 'single' && bounds(Math.fround(value))); break;
    }
    case 'integer': valid = Number.isInteger(value) && value >= -2147483648 && value <= 2147483647 && (control.minimum === undefined || value >= control.minimum) && (control.maximum === undefined || value <= control.maximum); break;
    case 'strings':
    case 'integers': {
      valid = Array.isArray(value) && value.length >= control.minItems && value.length <= control.maxItems;
      if (!valid) break;
      let bytes = 0;
      for (const item of value) {
        if (control.type === 'integers') {
          if (!Number.isInteger(item) || item < control.minimum || item > control.maximum) { valid = false; break; }
        } else {
          if (typeof item !== 'string' || item.length > control.maxUtf8Bytes) { valid = false; break; }
          for (const character of item) {
            const point = character.codePointAt(0);
            if (point >= 0xd800 && point <= 0xdfff) { valid = false; break; }
            bytes += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
          }
          if (!valid || bytes > control.maxUtf8Bytes) { valid = false; break; }
        }
      }
      if (control.contains !== undefined && !value.includes(control.contains)) valid = false;
      break;
    }
    default: valid = false;
  }
  if (!valid) fail(`Invalid ${control.label}`); return value;
}
export function parseSimulationArray(control,text) {
  if (!['strings','integers'].includes(control.type)) fail(`Invalid ${control.label}`);
  let value;
  try { value=JSON.parse(text); } catch { fail(`Invalid ${control.label}: enter a JSON array`); }
  return validateSimulationValue(control,value);
}
export class SimulationSetup {
  constructor(rpc,revokeReview) { this.rpc = rpc; this.revokeReview = revokeReview; this.busy = false; this.uncertain = false; }
  load(description,source,revision) {
    if (this.busy) fail('Simulation operation is still pending');
    this.controls = simulationControls(description,source); this.source = structuredClone(source); this.revision = revision; this.uncertain = false;
  }
  statusValue(status) {
    try {
      const groups = new Map();
      for (const control of this.controls.filter(c => c.path.length === 2)) {
        if (!groups.has(control.path[0])) groups.set(control.path[0], []);
        groups.get(control.path[0]).push(control.path[1]);
      }
      members(status,[...new Set(['deviceType','safe','switchValues','weather','fault','sampleAgeSeconds',...groups.keys()])]);
      if (status.deviceType !== this.source.backend.deviceType) protocol();
      for (const [group,keys] of groups) members(status[group],keys);
      for (const control of this.controls) {
        let value = status; for (const key of control.path) { if (!object(value) || !(key in value)) protocol(); value = value[key]; }
        validateSimulationValue(control,value);
      }
      if (status.filterWheel && (status.filterWheel.names.length !== status.filterWheel.focusOffsets.length || status.filterWheel.position >= status.filterWheel.names.length)) protocol();
      return status;
    } catch { protocol(); }
  }
  patch(selected) {
    if (!Array.isArray(selected) || selected.length === 0) fail('Select at least one simulation field to change');
    const patch = {}, paths = new Set();
    for (const {path,value} of selected) {
      const control = this.controls.find(c => JSON.stringify(c.path) === JSON.stringify(path));
      const key = path.join('/'); if (!control || paths.has(key)) fail('Invalid or duplicate simulation field'); paths.add(key);
      validateSimulationValue(control,value);
      if (path.length === 1) patch[path[0]] = value; else { patch[path[0]] ??= {}; patch[path[0]][path[1]] = value; }
    }
    return patch;
  }
  async read() { return await this.operation(async () => {
    const status = await this.rpc({op:'sourceStatus',source:this.source.id});
    if (status.source !== this.source.id || status.revision !== this.revision || status.simulated !== true) protocol();
    return this.statusValue(status.simulation);
  }); }
  async update(selected) {
    const update = this.patch(selected);
    return await this.operation(async () => {
      this.revokeReview();
      const result = await this.rpc({op:'updateSimulation',source:this.source.id,expectedRevision:this.revision,update});
      members(result,['source','configurationRevision','simulation']);
      if (result.source !== this.source.id || result.configurationRevision !== this.revision) protocol();
      return this.statusValue(result.simulation);
    });
  }
  async operation(work) {
    if (this.busy || this.uncertain || this.controls?.length === 0) fail('Reload before another simulation operation');
    this.busy = true;
    try { return await work(); }
    catch (error) {
      if (!error.detail || ['uncertain','revisionConflict','disconnected','unavailable','timeout','transient','responseTooLarge'].includes(error.detail.code)) {
        this.uncertain = true; error.uncertain = true;
        this.revokeReview();
      }
      throw error;
    } finally { this.busy = false; }
  }
}
