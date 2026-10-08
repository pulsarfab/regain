// Real browser actions against a caller-owned, loopback-only simulated hub.
// NODE_PATH may point at the bundled Playwright dependencies.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const path=require('node:path');
(async()=>{
  const base=process.argv[2], destination=process.argv[3];
  if (!base || new URL(base).hostname!=='127.0.0.1') throw Error('Use an explicitly simulated loopback server');
  const state=await fetch(base+'/setup/api/state').then(r=>r.json()); assert.equal(state.simulation,true);
  const browser=await chromium.launch({channel:'chrome',headless:true});
  try {
    const page=await browser.newPage({viewport:{width:1280,height:960}}), scriptErrors=[];
    page.on('pageerror',error=>scriptErrors.push(error.message));
    await page.goto(base+'/setup/hub'); await page.locator('#configuration-export:enabled').waitFor();
    const saved=await page.evaluate(async()=> (await (await fetch('/setup/api/hub',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({op:'getConfig'})})).json()).result);
    assert(saved.sources.every(s=>s.backend.kind==='simulated'));
    const downloading=page.waitForEvent('download'); await page.locator('#configuration-export').click();
    const download=await downloading, stream=await download.createReadStream(), parts=[];
    for await (const part of stream) parts.push(part);
    const text=Buffer.concat(parts).toString('utf8'), document=JSON.parse(text);
    assert.deepEqual(document.configuration,saved); assert.deepEqual(document.credentialSources,[]);
    await page.locator('#validate').click();await page.locator('#apply:enabled').waitFor();
    const duplicate='{"formatVersion":1,'+text.trim().slice(1);
    await page.locator('#import-file').setInputFiles({name:'duplicate.json',mimeType:'application/json',buffer:Buffer.from(duplicate)});
    await page.locator('#configuration-import').click(); await page.getByText('Invalid configuration file or duplicate object keys',{exact:false}).waitFor();
    assert.equal(await page.locator('#apply').isEnabled(),true);
    document.configuration.outputs[0].label='Imported browser controls [SIMULATION]';
    await page.locator('#import-file').setInputFiles({name:'restore.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(document))});
    await page.locator('#configuration-import').click();await page.locator('#transfer-result').filter({hasText:'Imported draft (restore)'}).waitFor();
    assert.equal(await page.locator('#apply').isEnabled(),false);
    const current=async()=>page.evaluate(async()=> (await (await fetch('/setup/api/hub',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({op:'getConfig'})})).json()).result);
    assert.deepEqual(await current(),saved);
    await page.locator('#validate').click();await page.locator('#apply:enabled').waitFor();await page.locator('#apply').click();
    await page.locator('#status').filter({hasText:'Saved revision'}).waitFor();assert.equal((await current()).outputs[0].label,'Imported browser controls [SIMULATION]');
    await page.locator('#import-file').setInputFiles({name:'copy.json',mimeType:'application/json',buffer:Buffer.from(text)});
    await page.locator('#import-mode').selectOption('copy');await page.locator('#configuration-import').click();
    await page.locator('#transfer-result').filter({hasText:'Changed device numbers: 3'}).waitFor();
    await page.locator('#validate').click();await page.locator('#apply:enabled').waitFor();
    const preview=JSON.parse(await page.locator('#preview-json').textContent());
    assert(preview.sources.every(s=>!saved.sources.some(old=>old.id===s.id)));
    assert.equal((await current()).outputs[0].label,'Imported browser controls [SIMULATION]');
    for (const source of saved.sources) {
      const status=await page.evaluate(async id=>(await (await fetch('/setup/api/hub',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({op:'sourceStatus',source:id})})).json()).result,source.id);
      assert.equal(status.leaseCount,0);
    }
    if (destination) {await fs.mkdir(destination,{recursive:true});await page.locator('#configuration-transfer').screenshot({path:path.join(destination,'hub-web-configuration-transfer-simulation.png')});}
    assert.deepEqual(scriptErrors,[]);console.log('Browser configuration transfer passed: real download/file upload, duplicate rejection, review invalidation, explicit apply, remapped copy, unchanged saved state and zero leases.');
  } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
