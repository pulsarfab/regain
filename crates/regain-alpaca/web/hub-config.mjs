// Generic reader for the Rust-generated configuration description. Device keys,
// labels, choices and defaults are data, not a second frontend definition.
export function configurationContract(description) {
  if (description?.contractVersion !== 1 || description?.schemaVersion !== 1 || !description.schema) {
    throw new Error('Unsupported hub configuration contract');
  }
  const root = description.schema;
  const resolve = node => {
    let result = node;
    const seen = new Set();
    while (result?.$ref) {
      const ref = result.$ref;
      if (!ref.startsWith('#/$defs/') || seen.has(ref)) throw new Error('Invalid configuration schema reference');
      seen.add(ref);
      const key = ref.slice('#/$defs/'.length).replaceAll('~1', '/').replaceAll('~0', '~');
      const target = root.$defs?.[key];
      if (!target) throw new Error('Unknown configuration schema reference');
      const { $ref, ...annotations } = result;
      result = { ...target, ...annotations };
    }
    return result;
  };
  const available = (node, capabilities = description.capabilities ?? []) => {
    const required = resolve(node)?.['x-regain']?.requiresCapability;
    return !required || capabilities.includes(required);
  };
  const variants = (node, capabilities) => {
    const alternatives = (resolve(node).oneOf ?? []).map(resolve);
    if (!alternatives.every(choice => typeof choice.properties?.kind?.const === 'string')) return [];
    return alternatives.map(choice => ({
    schema: choice,
    kind: choice.properties?.kind?.const,
    title: choice.title ?? choice.properties?.kind?.const,
    description: choice.description ?? '',
    enabled: available(choice, capabilities)
    }));
  };
  const choices = (node, capabilities = description.capabilities ?? []) => {
    const schema = resolve(node);
    if (!schema.enum && schema.oneOf) {
      const scalar = schema.oneOf.map(resolve);
      if (!scalar.every(choice => typeof choice.const === 'string')) return [];
      return scalar.map(choice => ({value:choice.const, enabled:available(choice, capabilities), description:choice.description ?? ''}));
    }
    return (schema.enum ?? []).map(value => ({value, enabled: !schema['x-regain']?.enumCapabilities?.[value] || capabilities.includes(schema['x-regain'].enumCapabilities[value])}));
  };
  const fields = (node, value = {}, capabilities, isNew = false) => {
    let schema = resolve(node);
    if (schema.oneOf) {
      const choice = variants(schema, capabilities).find(s => s.kind === value?.kind)?.schema;
      if (!choice) return [];
      schema = resolve(choice);
    }
    return Object.entries(schema.properties ?? {}).filter(([, s]) => !resolve(s)['x-regain']?.hidden).map(([key, raw]) => {
      const field = resolve(raw);
      return { key, schema: field, title: field.title ?? key, description: field.description ?? '',
        required: (schema.required ?? []).includes(key), enabled: available(field, capabilities),
        readOnly: field.readOnly === true || (field['x-regain']?.immutableAfterCreate === true && !isNew),
        value: Object.hasOwn(value, key) ? value[key] : field.default };
    });
  };
  return { root, resolve, variants, choices, fields, available };
}
