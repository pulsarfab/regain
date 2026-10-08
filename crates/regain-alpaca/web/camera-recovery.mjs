// Standalone editors use the same core schema as native hub camera setup.
export function recoveryFields(description, platform) {
  if (description?.contractVersion !== 1 || description.schema?.type !== 'object')
    throw Error('Unsupported camera recovery contract');
  return Object.entries(description.schema.properties).map(([key, schema]) => ({key, ...schema}))
    .filter(field => field['x-regain'].platforms.includes(platform))
    .sort((a,b) => a['x-regain'].order - b['x-regain'].order);
}

export function parseRecoveryValue(field, input) {
  const invalid = () => { throw Error(`Invalid ${field.title.toLowerCase()}. ${field.description}`); };
  if (field.type === 'boolean') return typeof input === 'boolean' ? input : invalid();
  if (typeof input !== 'string' || input.trim() === '') return invalid();
  const value = Number(input);
  if (!Number.isFinite(value) || (field.type === 'integer' && !Number.isInteger(value)) ||
      (field.minimum !== undefined && value < field.minimum) ||
      (field.exclusiveMinimum !== undefined && value <= field.exclusiveMinimum) ||
      (field.maximum !== undefined && value > field.maximum)) return invalid();
  return value;
}

export function renderRecovery(fields, document) {
  const sections = {Recovery:'retry-fields', Cooling:'cooling-fields', Timeouts:'timeout-fields'};
  for (const field of fields) {
    const metadata = field['x-regain'];
    const label = document.createElement('label');
    label.textContent = field.title + (metadata.units ? ` (${metadata.units})` : '');
    const input = document.createElement('input');
    input.id = field.key;
    input.title = field.description;
    if (field.type === 'boolean') {
      input.type = 'checkbox'; label.classList.add('check');
    } else {
      Object.assign(input, {type:'number', required:true, step:field.type === 'integer' ? '1' : 'any',
        min:field.minimum ?? field.exclusiveMinimum, max:field.maximum});
      input.addEventListener('input', () => {
        try { parseRecoveryValue(field,input.value); input.setCustomValidity(''); }
        catch (e) { input.setCustomValidity(e.message); }
      });
    }
    label.append(input);
    const hint = document.createElement('small'); hint.className = 'hint'; hint.textContent = field.description;
    label.append(hint);
    document.getElementById(sections[metadata.section]).append(label);
  }
}

export function readRecovery(fields, original, document) {
  const values = structuredClone(original);
  for (const field of fields) {
    const input = document.getElementById(field.key);
    values[field.key] = parseRecoveryValue(field, field.type === 'boolean' ? input.checked : input.value);
  }
  return values;
}
