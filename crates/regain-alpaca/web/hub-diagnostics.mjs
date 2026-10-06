// Reply shape comes from Rust. Frontends add saved identity/page fences; they
// display the host's decisions and never implement safety policy or retry it.
const protocol = () => { throw new Error('Invalid cached output diagnostic response'); };
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
export const equalDiagnosticValue = (a,b) => {
  if (Array.isArray(a)) return Array.isArray(b) && a.length === b.length && a.every((v,i)=>equalDiagnosticValue(v,b[i]));
  if (object(a)) return object(b) && Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(k=>Object.hasOwn(b,k) && equalDiagnosticValue(a[k],b[k]));
  return a === b;
};
export function validateDiagnosticSchema(schema,value) {
  const root = schema;
  function valid(node,value,depth=0) {
    if (depth > 32) return false;
    if (node.$ref) {
      if (!node.$ref.startsWith('#/$defs/')) return false;
      const target = root.$defs?.[node.$ref.slice(8).replaceAll('~1','/').replaceAll('~0','~')];
      if (!target) return false;
      if (!valid(target,value,depth+1)) return false;
    }
    if (node.oneOf && node.oneOf.filter(n=>valid(n,value,depth+1)).length !== 1) return false;
    if (node.anyOf && !node.anyOf.some(n=>valid(n,value,depth+1))) return false;
    if (node.allOf && !node.allOf.every(n=>valid(n,value,depth+1))) return false;
    if (Object.hasOwn(node,'const') && !equalDiagnosticValue(node.const,value)) return false;
    if (node.enum && !node.enum.some(v=>equalDiagnosticValue(v,value))) return false;
    const types = node.type ? [].concat(node.type) : [];
    if (types.length && !types.some(t=> t==='null' ? value===null : t==='object' ? object(value) : t==='array' ? Array.isArray(value) : t==='integer' ? Number.isSafeInteger(value) : t==='number' ? typeof value==='number' && Number.isFinite(value) : typeof value===t)) return false;
    if (typeof value==='number' && (!Number.isFinite(value) || node.minimum!==undefined && value<node.minimum || node.maximum!==undefined && value>node.maximum || node.exclusiveMinimum!==undefined && value<=node.exclusiveMinimum)) return false;
    if (typeof value==='string') {
      if (node.format==='uuid' && (!/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value) || /^0{8}(-0{4}){3}-0{12}$/.test(value))) return false;
      if (node.minLength!==undefined && [...value].length<node.minLength || node.maxLength!==undefined && [...value].length>node.maxLength || node.pattern && !(new RegExp(node.pattern)).test(value)) return false;
    }
    if (Array.isArray(value)) {
      if (value.length>4096 || node.maxItems!==undefined && value.length>node.maxItems || node.minItems!==undefined && value.length<node.minItems) return false;
      if (node.items && !value.every(v=>valid(node.items,v,depth+1))) return false;
      if (node.contains && !value.some(v=>valid(node.contains,v,depth+1))) return false;
    }
    if (object(value)) {
      if ((node.required??[]).some(k=>!Object.hasOwn(value,k))) return false;
      for (const [key,item] of Object.entries(value)) {
        if (node.properties?.[key]) { if (!valid(node.properties[key],item,depth+1)) return false; }
        else if (node.additionalProperties===false || object(node.additionalProperties) && !valid(node.additionalProperties,item,depth+1)) return false;
      }
    }
    return true;
  }
  if (!valid(root,value)) protocol();
}
const fail = message => { const error = new Error(message); error.detail={code:'invalidValue',message}; throw error; };
export class OutputDiagnostics {
  constructor(rpc,revokeReview=()=>{}) { this.rpc=rpc; this.revokeReview=revokeReview; this.busy=false; this.uncertain=false; this.observation=null; }
  load(description,saved) {
    if (this.busy) fail('Diagnostic request is still pending');
    const d=description.outputDiagnostics;
    if (d?.purpose!=='cachedDiagnostics' || d.opensSource!==false || d.writesEquipment!==false || d.countsSafetyObservations!==false || d.requiresRevision!==true || !Number.isInteger(d.deadlineSeconds) || d.deadlineSeconds<1 || d.deadlineSeconds>300 || !d.responseSchema) protocol();
    this.description=structuredClone(d); this.saved=structuredClone(saved); this.uncertain=false; this.observation=null;
  }
  parameter(key,value) {
    const field=this.description.parameters[key];
    if (field?.type!=='integer' || !Number.isSafeInteger(value) || value<field.minimum || value>field.maximum) fail(`Invalid ${field?.label??'diagnostic parameter'}`);
    return value;
  }
  validate(result,output,start,limit) {
    validateDiagnosticSchema(this.description.responseSchema,result);
    const kind=output.device.kind==='proxy' ? output.device.deviceType : output.device.kind;
    const type={switch:'switch',safety:'safetymonitor',weather:'observingconditions',focuser:'focuser',rotator:'rotator',filterwheel:'filterwheel'}[kind];
    if (result.purpose!=='cachedDiagnostics' || result.output!==output.id || result.configurationRevision!==this.saved.revision || result.deviceType!==type || result.observedSeconds<0 || result.start!==start || result.limit!==limit || result.total<start || result.total>1024 || result.diagnostics.kind!==kind) protocol();
    const end=Math.min(start+limit,result.total);
    if (result.nextStart!==(end<result.total ? end : null)) protocol();
    const d=result.diagnostics;
    const health = value => { if (value.revision!==this.saved.revision || !this.saved.sources.some(s=>s.id===value.source)) protocol(); validatePolling(value.polling); };
    if (d.kind==='switch') {
      const numbers=output.device.channels.map(c=>c.number);
      for (const entry of Object.values(this.saved.identities?.channels??{})) if (entry.output===output.id) numbers.push(entry.number);
      if (result.total!==(numbers.length ? Math.max(...numbers)+1 : 0) || d.channels.length!==end-start) protocol();
      d.channels.forEach((item,index)=>{
        const saved=output.device.channels.find(c=>c.number===start+index);
        if (item.number!==start+index || item.state!==(saved?'configured':'removed')) protocol();
        if (saved) {
          if (item.id!==saved.id || !equalDiagnosticValue(item.readout,saved.readout) || item.configuredWritable!==saved.writable) protocol();
          health(item.health); if (item.health.source!==saved.readout.source) protocol();
          if (item.sample.state==='available' && (item.sample.reading.source!==saved.readout.source || item.sample.reading.revision!==this.saved.revision || item.sample.reading.ageSeconds<0)) protocol();
        }
      });
    } else if (d.kind==='safety') {
      if (result.total!==output.device.members.length || d.members.length!==end-start || !d.controllerActive && d.isSafe) protocol();
      d.members.forEach((item,index)=>{
        const saved=output.device.members[start+index];
        if (item.source!==saved.source || item.enabled!==saved.enabled || saved.policy && !equalDiagnosticValue(item.policy,saved.policy) || item.enabled!==(item.decision!==null)) protocol();
        health(item.health); if (item.health.source!==item.source || item.decision && (item.decision.configurationRevision!==this.saved.revision || !d.controllerActive && (item.decision.permitsSafe || item.decision.rawIsSafe!==null))) protocol();
      });
    } else if (d.kind==='focuser' || d.kind==='rotator' || d.kind==='filterwheel') {
      const properties=this.description[`${d.kind}Properties`];
      if (!Array.isArray(properties) || properties.length!==result.total || d.properties.length!==end-start || d.health.source!==output.device.source) protocol();
      health(d.health);
      d.properties.forEach((item,index)=>{
        const field=properties[start+index]; if (item.property!==field.property) protocol();
        if (item.sample.state==='available') {
          const reading=item.sample.reading, value=reading.value;
          if (reading.source!==d.health.source || reading.generation!==d.health.generation || reading.revision!==this.saved.revision || reading.sequence>d.health.sequence || reading.ageSeconds<0 || value.type!==field.valueType || field.minimum!==null && value.value<field.minimum || field.exclusiveMinimum!==null && value.value<=field.exclusiveMinimum || field.maximum!=null && value.value>field.maximum || field.exclusiveMaximum!=null && value.value>=field.exclusiveMaximum) protocol();
        }
      });
    } else {
      const metrics=Object.keys(output.device.measurements).sort();
      if (result.total!==metrics.length || d.measurements.length!==end-start || d.averagePeriodHours<0) protocol();
      d.measurements.forEach((item,index)=>{
        const metric=metrics[start+index], saved=output.device.measurements[metric];
        if (item.metric!==metric || !equalDiagnosticValue(item.configuration,saved) || item.sources.length!==saved.sources.length) protocol();
        item.sources.forEach((value,i)=>{health(value); if(value.source!==saved.sources[i].source) protocol();});
        if (item.sample.state==='available' && (item.sample.reading.revision!==this.saved.revision || item.sample.reading.ageSeconds<0 || !saved.sources.some(s=>equalDiagnosticValue(s,item.sample.reading.readout)))) protocol();
      });
    }
    return result;
  }
  async read(id,start,limit) {
    if (this.busy || this.uncertain) fail('Reload before another diagnostic request');
    const output=this.saved.outputs.find(o=>o.id===id); if (!output) fail('Select a saved output');
    this.parameter('start',start); this.parameter('limit',limit);
    this.busy=true; this.observation=null;
    try {
      const result=this.validate(await this.rpc({op:'outputStatus',output:id,expectedRevision:this.saved.revision,start,limit},undefined,this.description.deadlineSeconds),output,start,limit);
      this.observation={kind:'cachedOutputHealth',observedAtUtc:new Date().toISOString(),instanceId:this.saved.instanceId,configurationRevision:this.saved.revision,result:structuredClone(result)};
      return result;
    } catch(error) {
      if (!error.detail || ['revisionConflict','disconnected','uncertain','timeout','unavailable'].includes(error.detail.code)) { this.uncertain=true; error.uncertain=true; this.revokeReview(); }
      throw error;
    } finally { this.busy=false; }
  }
}
export function diagnosticSummary(result) {
  const d=result.diagnostics, lines=[`${result.simulated?'Simulation · ':''}Cached ${d.kind} output · revision ${result.configurationRevision}`];
  if (d.kind==='safety') {
    lines.push(`${d.isSafe?'SAFE':'UNSAFE'} · ${d.controllerActive?'Controller active':'Controller inactive'}`);
    for (const m of d.members) {
      const s=m.decision; lines.push(`${m.source}: ${!m.enabled?'Disabled; no vote':`${s.phase} · raw ${s.rawIsSafe===null?'unknown':s.rawIsSafe?'safe':'unsafe'} · effective ${s.permitsSafe?'safe':'unsafe'} · ${s.reason}`}`);
      if(s) lines.push(`Failed checks ${s.failedCycles}/${m.policy.failedCyclesToUnsafe}; unsafe readings ${s.unsafeReadings}/${m.policy.unsafeReadingsToUnsafe}; recovery ${s.safeReadings}/${m.policy.safeReadingsToSafe} safe readings, ${s.safeHoldSeconds.toFixed(1)}/${m.policy.returnToSafeHoldSeconds} s hold; safe age ${s.safeAgeSeconds===null?'unknown':s.safeAgeSeconds.toFixed(1)+' s'}/${m.policy.maximumSafeAgeSeconds} s.`);
      if(m.health.writeUncertain) lines.push('Source has an uncertain write; reconcile equipment state before another command.');
      lines.push(pollingSummary(m.health));
    }
  } else if (d.kind==='focuser' || d.kind==='rotator' || d.kind==='filterwheel') {
    for (const item of d.properties) lines.push(item.sample.state==='available'
      ? `${item.property}: ${Array.isArray(item.sample.reading.value.value)?JSON.stringify(item.sample.reading.value.value):item.sample.reading.value.value} · age ${item.sample.reading.ageSeconds.toFixed(1)} s`
      : `${item.property}: unavailable · ${item.sample.error.message}`);
    if(d.health.writeUncertain) lines.push('Source has an uncertain write; reconcile equipment state before another command.');
    lines.push(pollingSummary(d.health));
  } else {
    const items=d.kind==='switch'?d.channels:d.measurements;
    for(const item of items) {
      if(item.state==='removed') {lines.push(`Channel ${item.number}: removed; number reserved.`); continue;}
      const sample=item.sample, label=d.kind==='switch'?`Channel ${item.number}: ${item.label}`:item.metric;
      lines.push(sample.state==='available'?`${label}: ${sample.reading.value} ${d.kind==='switch'?item.units:sample.reading.unit} · age ${sample.reading.ageSeconds.toFixed(1)} s`:`${label}: unavailable · ${sample.error.message}`);
      if(d.kind==='switch') lines.push(`Configured ${item.configuredWritable?'writable':'read-only'}; operational write capability is checked separately.${item.health.writeUncertain?' Retained uncertain write.':''}`);
      for (const health of d.kind==='switch'?[item.health]:item.sources) lines.push(pollingSummary(health));
    }
  }
  lines.push('Observed cache only. No equipment connection or safety confirmation is started.'); return lines.join('\n');
}
export function pollingSummary(health) {
  const p=health.polling;
  return `Source ${health.source}: polling ${p.phase}${p.reason===null?'':' · '+p.reason}. Host observation ${p.observedSeconds.toFixed(1)} s; ${p.nextPollAfterSeconds===null?'no scheduled wait reported':`next poll scheduled in ${p.nextPollAfterSeconds.toFixed(3)} s at that observation; actor work may delay it`}. Read attempts started ${p.attemptsStarted}/${p.attemptsPerCycle}; last ${p.lastAttempt}${p.lastCycleExhausted===null?'':p.lastCycleExhausted?' (cycle complete)':' (cycle pending)'}; backoff failures ${p.backoffFailures}.`;
}
export function validatePolling(p) {
  if (p.phase==='waiting' ? p.nextPollAfterSeconds===null || p.reason===null : p.nextPollAfterSeconds!==null) protocol();
  if (p.attemptsStarted>p.attemptsPerCycle || p.lastAttempt>p.attemptsPerCycle) protocol();
}
