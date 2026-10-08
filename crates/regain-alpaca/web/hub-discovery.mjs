import { validateDiagnosticSchema } from './hub-diagnostics.mjs';
import { initialValue, newIdentity } from './hub-form.mjs';

const invalid = message => { const error = new Error(message); error.detail = {code:'invalidValue',message}; throw error; };
function catalogIdentity(id) {
  const value=id.length===45 && id.startsWith('urn:uuid:')?id.slice(9):/^\{.{36}\}$/.test(id)?id.slice(1,-1):id;
  return /^([0-9a-f]{32}|[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12})$/i.test(value)?value.toLowerCase().replaceAll('-',''):id;
}
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
          const identity=catalogIdentity(device.uniqueId);
          if (!device.name.trim() || /\p{Cc}/u.test(device.name) || !device.uniqueId.trim() ||
              addresses.has(address) || identities.has(identity) || device.supportedDeviceType!==(known.includes(type)?type:null)) throw new Error();
          addresses.add(address); identities.add(identity);
        }
      } catch { throw new Error('Invalid Alpaca catalog response'); }
      this.catalog=structuredClone(result); this.credentialReference=credentialReference; return structuredClone(result);
    } catch (error) {
      if (!error.detail || ['revisionConflict','disconnected','invalidValue'].includes(error.detail.code)) {
        this.uncertain=true; error.uncertain=true;
      }
      throw error;
    } finally { this.busy=false; }
  }
  adopt(reader,draft,index,uuid=newIdentity) {
    if (this.busy || this.uncertain || !this.catalog || draft.revision!==this.revision) invalid('Query the current catalog before adding a source');
    if (!Number.isInteger(index) || index<0 || index>=this.catalog.devices.length) invalid('Select a catalog entry');
    const device=this.catalog.devices[index];
    if (device.supportedDeviceType===null) invalid('This device class is not supported by Regain Hub');
    const server=this.catalog.baseUrl, type=device.supportedDeviceType, identity=device.uniqueId;
    const array=reader.resolve(reader.root.properties.sources), schema=reader.resolve(array.items);
    if (draft.sources.length>=array.maxItems) invalid('Configuration source limit reached');
    for (const source of draft.sources) {
      const b=source.backend;
      if (b.kind==='alpaca' && (b.uniqueId!=null && catalogIdentity(b.uniqueId)===catalogIdentity(identity) ||
          serverUrl(b.baseUrl)===serverUrl(server) && b.deviceType===type && b.deviceNumber===device.number))
        invalid('This Alpaca device already has a source. Share its existing source ID');
    }
    const choice=reader.variants(schema.properties.backend).find(v=>v.kind==='alpaca' && v.enabled);
    if (!choice) invalid('Alpaca sources are unavailable in this host');
    const source=initialValue(reader,schema,uuid), backend=initialValue(reader,choice.schema,uuid);
    backend.baseUrl=server; backend.deviceType=type; backend.deviceNumber=device.number; backend.uniqueId=identity;
    if (this.credentialReference!==null) backend.credentialReference=this.credentialReference;
    source.backend=backend;
    source.label=[...device.name].slice(0,schema.properties.label.maxLength).join('');
    draft.sources.push(source); return source.id;
  }
}

export function catalogSummary(catalog) {
  if (!catalog.devices.length) return 'The server reports no configured devices.';
  return catalog.devices.map(device=>`${device.name} — ${device.reportedDeviceType} ${device.number}\nID: ${device.uniqueId}${device.supportedDeviceType===null?'\nThis device class is not supported by Regain Hub.':''}`).join('\n\n');
}

export class AlpacaNetworkDiscovery {
  constructor(rpc) { this.rpc=rpc; this.busy=false; this.uncertain=false; this.result=null; }
  load(description,saved) {
    if (this.busy) invalid('A network search is still pending');
    this.description=description.discovery.network; this.revision=saved.revision; this.result=null; this.uncertain=false;
    const d=this.description;
    if (['opensSource','writesEquipment','persistsConfiguration','readsCatalog','usesCredentials'].some(key=>d[key]!==false) ||
        !Number.isInteger(d.timeoutSeconds) || d.timeoutSeconds<1 || d.timeoutSeconds>300) throw new Error('Invalid network discovery description');
  }
  async search() {
    if (this.busy || this.uncertain || !this.description) invalid('Reload before searching for servers');
    this.busy=true; this.result=null; const d=this.description;
    try {
      const result=await this.rpc({op:d.operation,expectedRevision:this.revision},undefined,d.timeoutSeconds+5);
      try {
        validateDiagnosticSchema(d.responseSchema,result);
        if (result.configurationRevision!==this.revision || result.interfacesFailed>result.interfacesTried ||
            result.interfacesFailed && !result.incomplete) throw new Error();
        const endpoints=new Set();
        for (const server of result.servers) {
          const {address,scopeId,port,baseUrl,unavailableReason}=server, ipv6=address.includes(':');
          const authority=ipv6?`[${address}]`:address, parsed=new URL(`http://${authority}:${port}`);
          if (address.includes('%') || parsed.hostname!==authority || address==='::' || /^::ffff:/i.test(address) ||
              (ipv6?parseInt(address.split(':')[0],16)>=0xff00:!/^\d+\.\d+\.\d+\.\d+$/.test(address) ||
                Number(address.split('.')[0])===0 || Number(address.split('.')[0])>=224 && Number(address.split('.')[0])<=239 || address==='255.255.255.255')) throw new Error();
          const linkLocal=ipv6 && parseInt(address.split(':')[0],16)>=0xfe80 && parseInt(address.split(':')[0],16)<=0xfebf;
          if (linkLocal ? scopeId===0 || baseUrl!==null || unavailableReason!=='scopedIpv6RequiresTransportSupport' :
              scopeId!==0 || unavailableReason!==null || baseUrl!==`http://${authority}:${port}`) throw new Error();
          const key=`${address}%${scopeId}:${port}`;
          if (endpoints.has(key)) throw new Error(); endpoints.add(key);
        }
      } catch { throw new Error('Invalid Alpaca network search response'); }
      this.result=structuredClone(result); return structuredClone(result);
    } catch (error) {
      if (!error.detail || ['revisionConflict','disconnected','invalidValue'].includes(error.detail.code)) {
        this.uncertain=true; error.uncertain=true;
      }
      throw error;
    } finally { this.busy=false; }
  }
}
