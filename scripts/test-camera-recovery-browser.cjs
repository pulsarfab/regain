// Real browser edit/save; caller owns an explicitly simulated loopback server.
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const path=require('node:path');
(async()=>{
  const base=process.argv[2], output=process.argv[3];
  if(!base || new URL(base).hostname!=='127.0.0.1') throw Error('Use a private loopback simulation');
  const state=await fetch(base+'/setup/api/state').then(r=>r.json());assert.equal(state.simulation,true);
  const browser=await chromium.launch({channel:'chrome',headless:true});
  try {
    const page=await browser.newPage({viewport:{width:1280,height:960}}),errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto(base+'/setup');await page.locator('#version').filter({hasText:'0.6'}).waitFor();
    if(!state.cameras.length) {await page.locator('#add').click();await page.locator('#save:visible').waitFor();}
    const latest=await fetch(base+'/setup/api/state').then(r=>r.json());
    const slot=latest.cameras[0].slot, profile=structuredClone(latest.cameras[0].profile);
    assert.equal(profile.camera,null);assert.equal(latest.cameras[0].connected,false);
    profile.recovery.usbPortCycle=true;
    const seeded=await fetch(base+`/setup/api/cameras/${slot}`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(profile)});
    assert.equal(seeded.ok,true);await page.reload();
    await page.locator('[data-tab="recovery"]').click();
    await page.locator('#reconnectDelaySeconds').fill('0.000001');
    assert.equal(await page.locator('#reconnectDelaySeconds').evaluate(e=>e.checkValidity()),true);
    await page.locator('#maxRetries').fill('1.5');
    assert.equal(await page.locator('#maxRetries').evaluate(e=>e.checkValidity()),false);
    await page.locator('#maxRetries').fill('0');
    await page.locator('#reconnectDelaySeconds').fill('0');
    assert.equal(await page.locator('#reconnectDelaySeconds').evaluate(e=>e.checkValidity()),false);
    await page.locator('#reconnectDelaySeconds').fill('0.000001');
    await page.locator('#save').click();await page.getByText('Settings saved.',{exact:true}).waitFor();
    const saved=(await fetch(base+'/setup/api/state').then(r=>r.json())).cameras[0];
    assert.equal(saved.connected,false);assert.equal(saved.profile.camera,null);
    assert.equal(saved.profile.recovery.reconnectDelaySeconds,.000001);assert.equal(saved.profile.recovery.maxRetries,0);
    assert.equal(saved.profile.recovery.usbPortCycle,true);
    const metadata=await fetch(base+'/setup/api/camera-recovery').then(r=>r.json());
    if(metadata.platform==='windows') assert.equal(await page.locator('#usbPortCycle').count(),0);
    assert.equal(await page.locator('#retry-fields input').count(),metadata.platform==='linux'?7:6);
    if(output){
      await page.locator('#reconnectDelaySeconds').fill('5');await page.locator('#maxRetries').fill('3');
      await page.locator('#save').click();await page.getByText('Settings saved.',{exact:true}).waitFor();
      await fs.mkdir(output,{recursive:true});await page.screenshot({path:path.join(output,'camera-recovery-web.png'),fullPage:true});
    }
    await page.route('**/setup/api/camera-recovery',route=>route.fulfill({json:{contractVersion:2,schema:{type:'object'}}}));
    await page.reload();await page.getByText('Unsupported camera recovery contract',{exact:true}).waitFor();
    for(const id of ['save','add','scan']) assert.equal(await page.locator('#'+id).isDisabled(),true);
    assert.deepEqual(errors,[]);
    console.log('Real browser: generated recovery fields, strict positive/integer limits, save, hidden Linux value, unknown-contract disable and no camera connection pass.');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
