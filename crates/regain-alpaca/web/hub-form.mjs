// Schema-driven editor. Values are drafts; only the host authorizes a save.
export function newIdentity() {
  // randomUUID is restricted to secure contexts; LAN HTTP still exposes the
  // cryptographic getRandomValues API. Never fall back to Math.random.
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64; bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, v => v.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
export function initialValue(reader, raw, uuid = newIdentity) {
  const schema = reader.resolve(raw);
  if (Object.hasOwn(schema, 'default')) return structuredClone(schema.default);
  if (Object.hasOwn(schema, 'const')) return schema.const;
  if (schema.oneOf) {
    const scalar = reader.choices(schema);
    if (scalar.length) {
      const choice = scalar.find(v => v.enabled);
      if (!choice) throw new Error('No supported configuration choice');
      return choice.value;
    }
    const choice = reader.variants(schema).find(v => v.enabled);
    if (!choice) throw new Error('No supported configuration choice');
    return initialValue(reader, choice.schema, uuid);
  }
  if (schema.anyOf) return initialValue(reader, schema.anyOf.find(v => v.type !== 'null'), uuid);
  if (schema.format === 'uuid') return schema.readOnly ? uuid() : '';
  if (schema.enum) return reader.choices(schema).find(choice => choice.enabled)?.value ?? '';
  const type = Array.isArray(schema.type) ? schema.type.find(v => v !== 'null') : schema.type;
  if (type === 'array') return [];
  if (type === 'object') {
    const value = {};
    for (const [key, field] of Object.entries(schema.properties ?? {})) {
      if (reader.resolve(field)['x-regain']?.hidden) continue;
      if ((schema.required ?? []).includes(key) || Object.hasOwn(reader.resolve(field), 'default')) value[key] = initialValue(reader, field, uuid);
    }
    return value;
  }
  if (type === 'boolean') return false;
  if (type === 'number' || type === 'integer') return schema.minimum ?? 0;
  return '';
}

export function previewValue(reader, raw, value) {
  let schema = reader.resolve(raw);
  const metadata = schema['x-regain'];
  if (metadata?.hidden || metadata?.export === 'omit' || metadata?.sensitive) return undefined;
  if (value == null) return value;
  if (schema.oneOf && reader.variants(schema).length) schema = reader.resolve(reader.variants(schema).find(v => v.kind === value.kind)?.schema ?? {});
  if (schema.anyOf) schema = reader.resolve(schema.anyOf.find(v => v.type !== 'null'));
  if (Array.isArray(value)) return value.map(item => previewValue(reader, schema.items ?? {}, item));
  if (typeof value === 'object') return Object.fromEntries(Object.entries(value).flatMap(([key,item]) => {
    if (!schema.properties?.[key]) return [];
    const preview = previewValue(reader, schema.properties[key], item);
    return preview === undefined ? [] : [[key,preview]];
  }));
  return value;
}

export function renderConfiguration(container, reader, draft, base, changed) {
  const openPaths = new Set([...container.querySelectorAll('details[open]')].map(node => node.dataset.path));
  const el = (tag, text) => { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; return node; };
  const button = (title, action) => { const node = el('button', title); node.type = 'button'; node.onclick = action; return node; };
  const redraw = () => { renderConfiguration(container, reader, draft, base, changed); changed(); };
  function field(raw, value, original, set, path, title, required = true) {
    let schema = reader.resolve(raw);
    const group = el('div'); group.className = 'hub-field'; group.dataset.path = path;
    if (schema['x-regain']?.hidden) return group;
    const heading = el('label', title ?? schema.title ?? path);
    group.append(heading);
    if (schema.description) { const note = el('p', schema.description); note.className = 'hint'; group.append(note); }
    const immutable = schema.readOnly || (schema['x-regain']?.immutableAfterCreate && original !== undefined);
    if (immutable) {
      if (schema.format === 'uuid') {
        const details = el('details'); details.dataset.path = path; details.open = openPaths.has(path); details.append(el('summary',title ?? schema.title ?? 'Identity'), el('p',schema.description ?? ''), el('code',String(value ?? ''))); group.replaceChildren(details);
      } else group.append(el('code', String(value ?? '')));
      return group;
    }
    if (!required && value === undefined) {
      group.append(button(Object.hasOwn(schema, 'default') ? 'Use default / customize' : 'Add', () => { set(initialValue(reader, schema)); redraw(); }));
      return group;
    }
    if (!required) group.append(button('Remove optional value', () => { set(undefined); redraw(); }));
    if (value === null) {
      group.append(el('span', 'Not set'), button('Set value', () => { const {default: ignored, ...withoutDefault} = schema; set(initialValue(reader, withoutDefault)); redraw(); }));
      return group;
    }
    if (schema.anyOf) schema = reader.resolve(schema.anyOf.find(v => v.type !== 'null'));
    if (schema.oneOf && reader.variants(schema).length) {
      const variants = reader.variants(schema);
      const select = el('select'); heading.append(select);
      for (const variant of variants) {
        const option = el('option', variant.title + (variant.enabled ? '' : ' (not available)'));
        option.value = variant.kind; option.disabled = !variant.enabled; select.append(option);
      }
      select.value = value?.kind ?? '';
      select.onchange = () => {
        const variant = variants.find(v => v.kind === select.value && v.enabled);
        if (!variant) throw new Error('Choose an available configuration type');
        set(initialValue(reader, variant.schema)); redraw();
      };
      const selected = schema.oneOf.find(v => v.properties?.kind?.const === value?.kind);
      if (!selected) { group.append(el('p', 'Choose a supported type.')); return group; }
      schema = reader.resolve(selected);
    }
    const type = Array.isArray(schema.type) ? schema.type.find(v => v !== 'null') : schema.type;
    if (type === 'array') {
      for (const [index, item] of (value ?? []).entries()) {
        const prior = item?.id ? original?.find(v => v.id === item.id) : original?.[index];
        const box = el('details');
        box.dataset.path = `${path}[${item?.id ?? index}]`; box.open = prior === undefined || openPaths.has(box.dataset.path);
        box.append(el('summary', item?.label || `${title ?? 'Item'} ${index + 1}`));
        box.append(field(schema.items, item, prior, v => { value[index] = v; }, `${path}[${index}]`, 'Settings'));
        box.append(button('Remove item', () => { value.splice(index, 1); redraw(); }));
        group.append(box);
      }
      const add = button('Add item', () => { value.push(initialValue(reader, schema.items)); redraw(); });
      add.disabled = (value?.length ?? 0) >= (schema.maxItems ?? Infinity);
      group.append(add); return group;
    }
    if (type === 'object') {
      for (const descriptor of reader.fields(schema, value, undefined, original === undefined)) {
        if (Object.hasOwn(descriptor.schema, 'const')) continue;
        const key = descriptor.key;
        group.append(field(descriptor.schema, value?.[key], original?.[key], v => {
          if (v === undefined) delete value[key]; else value[key] = v;
        }, path ? `${path}.${key}` : key, descriptor.title, descriptor.required));
      }
      return group;
    }
    let input;
    const reference = schema['x-regain']?.reference;
    if (reference || reader.choices(schema).length) {
      input = el('select'); input.append(el('option', ''));
      const choices = reference ? (reference === 'source' ? draft.sources : draft.outputs).map(v => ({value:v.id, label:`${v.label} (${v.id})`, enabled:true})) : reader.choices(schema).map(v => ({...v, label:v.value + (v.enabled ? '' : ' (not available)')}));
      if (value && !choices.some(v => v.value === value)) choices.unshift({value, label:`Unavailable: ${value}`});
      for (const choice of choices) { const option = el('option', choice.label); option.value = choice.value; option.disabled = choice.enabled === false; option.title = choice.description ?? ''; input.append(option); }
      input.value = value ?? '';
      input.onchange = () => { set(input.value); changed(); };
    } else {
      input = el('input'); input.type = type === 'boolean' ? 'checkbox' : ['integer','number'].includes(type) ? 'number' : 'text';
      if (type === 'boolean') input.checked = value ?? schema.default ?? false;
      else input.value = value ?? schema.default ?? '';
      if (input.type === 'number') {
        input.step = type === 'integer' ? '1' : 'any';
        if (schema.minimum !== undefined) input.min = schema.minimum;
        if (schema.maximum !== undefined) input.max = schema.maximum;
      }
      if (schema.maxLength) input.maxLength = schema.maxLength;
      input.oninput = () => {
        if (input.type === 'number' && (input.value === '' || !Number.isFinite(input.valueAsNumber))) { input.setCustomValidity('Enter a finite number.'); changed(); return; }
        input.setCustomValidity('');
        set(type === 'boolean' ? input.checked : input.type === 'number' ? input.valueAsNumber : input.value); changed();
      };
    }
    input.disabled = !reader.available(schema);
    // JSON Schema required means property presence; strings such as unit labels
    // may legitimately be empty unless the schema says minLength > 0.
    input.required = required && (input.tagName === 'SELECT' || input.type === 'number' || (schema.minLength ?? 0) > 0);
    input.setAttribute('aria-label', `${title ?? path} · ${path}`);
    heading.append(input);
    if (schema['x-regain']?.units) heading.append(el('span', schema['x-regain'].units));
    return group;
  }
  container.replaceChildren(field(reader.root, draft, base, () => {}, '', 'Hub configuration'));
}
