import { validateDiagnosticSchema } from './hub-diagnostics.mjs';

const invalid = message => { const error = new Error(message); error.detail = {code:'invalidValue',message}; throw error; };
function serverUrl(text) {
  let url; try { url = new URL(text); } catch { invalid('Enter an HTTP or HTTPS server URL'); }
  if (!['http:','https:'].includes(url.protocol) || url.username || url.password || text.includes('?') || text.includes('#'))
    invalid('Enter a server URL without credentials, query or fragment');
  return url.href.replace(/\/+$/,'');
}
export class AlpacaDiscovery {
  constructor(rpc) { this.rpc=rpc; this.busy=false; this.catalog=null; this.uncertain=false; }
  load(description,saved) {
    if (this.busy) invalid('A catalog query is still pending');
    this.description=description.discovery.alpaca; this.revision=saved.revision; this.catalog=null; this.uncertain=false;
    const d=this.description;
    if (d.opensSource!==false || d.writesEquipment!==false || d.persistsConfiguration!==false ||
        !Number.isInteger(d.timeoutSeconds) || d.timeoutSeconds<1 || d.timeoutSeconds>300)
      throw new Error('Invalid catalog discovery description');
  }
  async query(baseUrl,credentialReference=null) {
    if (this.busy || this.uncertain || !this.description) invalid('Reload before querying a catalog');
    const d=this.description;
    if (typeof baseUrl!=='string' || baseUrl.length>d.parameters.baseUrl.maxLength) invalid('Invalid server URL');
    const requested=serverUrl(baseUrl);
    this.busy=true; this.catalog=null;
    try {
      const result=await this.rpc({op:d.operation,baseUrl,credentialReference,expectedRevision:this.revision},undefined,d.timeoutSeconds+5);
      try {
        validateDiagnosticSchema(d.responseSchema,result);
        if (result.configurationRevision!==this.revision || serverUrl(result.baseUrl)!==requested) throw new Error();
        const addresses=new Set(), identities=new Set(), known=d.responseSchema.$defs.DeviceType.enum;
        for (const device of result.devices) {
          const type=device.reportedDeviceType.toLowerCase(), address=`${type}:${device.number}`;
          const uuid=device.uniqueId.replace(/^urn:uuid:/,'').replace(/^\{(.*)\}$/,'$1');
          const identity=/^([0-9a-f]{32}|[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12})$/i.test(uuid) ? uuid.toLowerCase().replaceAll('-','') : device.uniqueId;
          if (!device.name.trim() || /\p{Cc}/u.test(device.name) || !device.uniqueId.trim() ||
              addresses.has(address) || identities.has(identity) || device.supportedDeviceType!==(known.includes(type)?type:null)) throw new Error();
          addresses.add(address); identities.add(identity);
        }
      } catch { throw new Error('Invalid Alpaca catalog response'); }
      this.catalog=structuredClone(result); return structuredClone(result);
    } catch (error) {
      if (!error.detail || ['revisionConflict','disconnected','invalidValue'].includes(error.detail.code)) {
        this.uncertain=true; error.uncertain=true;
      }
      throw error;
    } finally { this.busy=false; }
  }
}

export function catalogSummary(catalog) {
  if (!catalog.devices.length) return 'The server reports no configured devices.';
  return catalog.devices.map(device=>`${device.name} — ${device.reportedDeviceType} ${device.number}\nID: ${device.uniqueId}${device.supportedDeviceType===null?'\nThis device class is not supported by Regain Hub.':''}`).join('\n\n');
}
