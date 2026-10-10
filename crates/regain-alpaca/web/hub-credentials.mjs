import { newIdentity } from './hub-form.mjs';

const control = /[\u0000-\u001f\u007f-\u009f]/u;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
function invalid() { throw new Error('Invalid credential metadata or response'); }
function reject(message, code = 'invalidValue') { const error = new Error(message); error.detail = {code}; throw error; }
function members(value, required, optional = []) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      required.some(key => !Object.hasOwn(value,key)) || Object.keys(value).some(key => !required.includes(key) && !optional.includes(key))) invalid();
}
export function credentialContract(storage) {
  if (storage == null) return null;
  const input = storage.input?.authorization, reference = storage.reference;
  if (!input || input.type !== 'string' || input.writeOnly !== true || input.sensitive !== true ||
      !Number.isInteger(input.maxLength) || input.maxLength < 1 || input.maxLength > 8192 ||
      !reference || reference.type !== 'string' || !Number.isInteger(reference.maxLength) || reference.maxLength < 1 || reference.maxLength > 200 ||
      [input.label,input.description,reference.label,reference.description,storage.protection,storage.protectionDescription,storage.rotation].some(value => typeof value !== 'string') ||
      storage.clientChosenReferences === true && (typeof storage.referencePrefix !== 'string' || control.test(storage.referencePrefix))) invalid();
  return structuredClone(storage);
}

// Only references and public metadata survive an operation. Callers clear their
// password input before create; this does not promise erasure of JS/OS copies.
export class CredentialSetup {
  constructor(send, referenceChanged = () => {}, invalidateReview = () => {}, makeId = newIdentity) {
    this.send = send; this.referenceChanged = referenceChanged; this.invalidateReview = invalidateReview; this.makeId = makeId;
    this.reference = ''; this.description = null; this.uncertain = false; this.busy = false;
  }
  load(storage) {
    if (this.busy) reject('Credential operation is still running','busy');
    this.description = credentialContract(storage); this.uncertain = false;
  }
  setReference(reference) {
    if (this.busy) reject('Credential operation is still running','busy');
    this.reference = reference;
  }
  validateReference() {
    if (!this.description || typeof this.reference !== 'string' || !this.reference.trim() ||
        Array.from(this.reference).length > this.description.reference.maxLength || control.test(this.reference) || /[\ud800-\udfff]/u.test(this.reference.replace(/[\ud800-\udbff][\udc00-\udfff]/g,'')))
      reject('Invalid credential reference');
  }
  async create(authorization) {
    if (this.busy || this.uncertain) reject('Reload and check the retained reference before another credential change',this.busy ? 'busy' : 'uncertain');
    const input = this.description?.input.authorization;
    if (!input || this.description.clientChosenReferences !== true) reject('This host does not support recoverable credential creation','unsupported');
    if (typeof authorization !== 'string' || !authorization.trim() || authorization.length > input.maxLength || /[^\x20-\x7e]/u.test(authorization))
      reject('Invalid authorization value');
    const id = this.makeId();
    if (!uuid.test(id) || /^0{8}-0{4}-0{4}-0{4}-0{12}$/.test(id)) invalid();
    this.reference = this.description.referencePrefix + id; this.validateReference();
    this.referenceChanged(this.reference);
    return this.operation({op:'createCredential', referenceId:id, authorization}, true);
  }
  status() { this.validateReference(); return this.operation({op:'credentialStatus',reference:this.reference},false); }
  remove() { this.validateReference(); return this.operation({op:'deleteCredential',reference:this.reference},true); }
  async operation(command, mutation) {
    if (this.busy || this.uncertain) reject('Reload and check the retained reference before another credential operation',this.busy ? 'busy' : 'uncertain');
    this.busy = true;
    try {
      if (mutation) this.invalidateReview();
      const result = await this.send(command);
      if (command.op === 'deleteCredential') {
        members(result,['removed'],['persistenceWarning']);
        if (typeof result.removed !== 'boolean' || Object.hasOwn(result,'persistenceWarning') && typeof result.persistenceWarning !== 'string') invalid();
      } else {
        members(result,['reference','present','protection']);
        if (result.reference !== this.reference || typeof result.present !== 'boolean' || mutation && !result.present || result.protection !== this.description.protection) invalid();
      }
      return result;
    } catch (error) {
      if (!error.detail || ['uncertain','disconnected','unavailable','timeout'].includes(error.detail.code)) this.uncertain = true;
      throw error;
    } finally { this.busy = false; }
  }
}
