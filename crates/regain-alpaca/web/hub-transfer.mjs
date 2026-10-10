import { validateDiagnosticSchema } from './hub-diagnostics.mjs';

const fail=message=>{throw new Error(message);};
function equal(a,b) {
  if (a===b) return true;
  if (a===null || b===null || typeof a!==typeof b || typeof a!=='object' || Array.isArray(a)!==Array.isArray(b)) return false;
  const keys=Object.keys(a); return keys.length===Object.keys(b).length && keys.every(key=>Object.hasOwn(b,key) && equal(a[key],b[key]));
}
const boundSources=config=>config.sources.filter(s=>s.backend.kind==='alpaca' && s.backend.credentialReference!=null).map(s=>s.id);
function unique(values) { return new Set(values).size===values.length; }
export function transferSummary(result) {
  return [`Imported draft (${result.mode}). Saved configuration is unchanged.`,
    `New IDs: ${result.remappedIds.length}. Changed device numbers: ${result.renumberedOutputs.length}.`,
    ...result.renumberedOutputs.map(n=>`${n.deviceType} ${n.output}: ${n.original} → ${n.replacement}`),
    `Retained local credential bindings: ${result.preservedCredentials.length}.`,
    ...(result.missingCredentials.length?['Supply credential bindings for sources:',...result.missingCredentials]:[]),
    'Review the draft and Apply separately.'].join('\n');
}
export class ConfigurationTransfer {
  constructor(rpc) { this.rpc=rpc; this.busy=false; }
  load(description,saved) {
    if (this.busy) fail('A configuration transfer is still pending');
    const d=description.configurationTransfer;
    if (!d || ['persistsConfiguration','opensSource','writesEquipment'].some(key=>d[key]!==false) ||
        d.exportOperation!=='exportConfig' || d.importOperation!=='prepareImport' || d.formatVersion!==1 ||
        !Number.isSafeInteger(d.maximumDocumentBytes) || d.maximumDocumentBytes<1 || d.maximumDocumentBytes>4194304 ||
        !equal(d.modes.map(m=>m.value),['restore','copy'])) fail('Invalid configuration transfer description');
    this.description=d; this.schema=description.schema; this.saved=structuredClone(saved);
  }
  async export() {
    if (this.busy || !this.description) fail('Reload before exporting configuration');
    this.busy=true;
    try {
      const document=await this.rpc({op:this.description.exportOperation,expectedRevision:this.saved.revision});
      validateDiagnosticSchema(this.description.documentSchema,document);
      if (document.configuration.revision!==this.saved.revision || document.configuration.instanceId!==this.saved.instanceId ||
          boundSources(document.configuration).length || !unique(document.credentialSources) ||
          !equal([...document.credentialSources].sort(),boundSources(this.saved).sort())) fail('Invalid redacted configuration export');
      return JSON.stringify(document,null,2);
    } finally { this.busy=false; }
  }
  async prepare(document,mode) {
    if (this.busy || !this.description) fail('Reload before importing configuration');
    const d=this.description;
    if (typeof document!=='string' || new TextEncoder().encode(document).length>d.maximumDocumentBytes || !d.modes.some(m=>m.value===mode))
      fail('Invalid configuration file or import mode');
    this.busy=true;
    try {
      // Send the original text: parsing and reserializing would hide duplicate keys.
      const result=await this.rpc({op:d.importOperation,expectedRevision:this.saved.revision,document,mode});
      validateDiagnosticSchema(d.responseSchema,result);
      validateDiagnosticSchema(this.schema,result.candidate);
      const candidate=result.candidate, input=JSON.parse(document.replace(/^\uFEFF/,''));
      if (result.mode!==mode || result.configurationRevision!==this.saved.revision || candidate.revision!==this.saved.revision ||
          candidate.instanceId!==this.saved.instanceId || result.sourceInstanceId!==input.configuration.instanceId ||
          !equal(candidate.identities,this.saved.identities) || !unique(result.preservedCredentials) || !unique(result.missingCredentials) ||
          result.missingCredentials.some(id=>result.preservedCredentials.includes(id))) fail('Invalid configuration import response');
      const bound=boundSources(candidate);
      if (!equal(bound.sort(),[...result.preservedCredentials].sort())) fail('Invalid imported credential bindings');
      for (const id of [...result.preservedCredentials,...result.missingCredentials]) {
        const source=candidate.sources.find(s=>s.id===id);
        if (!source || source.backend.kind!=='alpaca') fail('Invalid imported credential source');
      }
      for (const id of result.preservedCredentials) {
        const current=this.saved.sources.find(s=>s.id===id), next=candidate.sources.find(s=>s.id===id);
        if (mode!=='restore' || current?.backend.kind!=='alpaca' || next.backend.credentialReference!==current.backend.credentialReference)
          fail('Invalid imported credential binding');
      }
      if (mode==='restore' && (result.sourceInstanceId!==this.saved.instanceId || result.remappedIds.length || result.renumberedOutputs.length) ||
          mode==='copy' && (bound.length || !unique(result.remappedIds.map(m=>m.original)) || !unique(result.remappedIds.map(m=>m.replacement)) ||
            result.remappedIds.some(m=>m.original===m.replacement))) fail('Invalid imported identities');
      return structuredClone(result);
    } catch (error) {
      if (!error.detail || ['revisionConflict','disconnected','unavailable'].includes(error.detail.code)) error.uncertain=true;
      throw error;
    } finally { this.busy=false; }
  }
}
