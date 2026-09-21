// Read-only capture of the production setup page with a physical FocusCube3.
const {chromium} = require('playwright');
const path = require('node:path');
(async()=>{
  const [base,serial,output] = process.argv.slice(2);
  if(!base || !serial || !output || !['127.0.0.1','localhost'].includes(new URL(base).hostname)) throw Error('Provide local server, serial, output');
  const browser=await chromium.launch({channel:'chrome',headless:true});
  const page=await browser.newPage({viewport:{width:1280,height:1100},deviceScaleFactor:1});
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  let connected=false;
  try {
    const rows=await (await page.request.get(base+'/setup/api/focusers')).json();
    const matches=rows.filter(r=>r.kind==='fc3'&&r.profile.serial===serial);
    if(matches.length!==1)throw Error('Configure exactly one FocusCube3 slot with this serial before capturing');
    const slot=matches[0].slot;
    await page.goto(base+`/setup/v1/focuser/${slot}/setup`);
    await page.getByText('Disconnected',{exact:true}).waitFor();
    await page.locator('#scan').click();
    await page.locator('#device option').filter({hasText:serial}).waitFor({state:'attached'});
    await page.waitForFunction(()=>!busy);
    if(await page.locator('#device').inputValue() !== serial) await Promise.all([page.waitForResponse(r=>r.url().endsWith(`/setup/api/focusers/${slot}`)&&r.request().method()==='POST'),page.locator('#device').selectOption(serial)]);
    await page.waitForFunction(()=>!busy);
    await page.getByRole('button',{name:'Connect for setup',exact:true}).click();connected=true;
    await page.getByText('Connected for setup',{exact:true}).waitFor();
    await page.waitForFunction(()=>document.getElementById('position').textContent.includes('Idle'));
    if(await page.locator('#simulation').isVisible()) throw Error('Physical device required for this screenshot');
    if(!(await page.locator('#speed').isVisible()) || await page.locator('#limit').isVisible()) throw Error('Incorrect FocusCube3 controls');
    if(errors.length) throw Error(errors.join('\n'));
    await page.screenshot({path:path.resolve(output),fullPage:true});
    console.log('Captured physical FocusCube3: '+output);
  } finally {
    try {if(connected){await page.evaluate(async()=>{while(busy) await new Promise(r=>setTimeout(r,25));document.getElementById('connect').click();});await page.getByRole('button',{name:'Connect for setup',exact:true}).waitFor();}}
    finally{await browser.close();}
  }
})().catch(e=>{console.error(e);process.exitCode=1;});
