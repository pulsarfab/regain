// Capture production OFP2 setup with the real panel or explicit simulation.
// Usage: node scripts/screenshot-ofp2.cjs BASE_URL USB_SERIAL OUTPUT_PATH
const {chromium} = require('playwright');
const path = require('node:path');
const fs = require('node:fs/promises');

(async () => {
  const [base, serial, output] = process.argv.slice(2);
  if (!base || !serial || !output || !['127.0.0.1','localhost'].includes(new URL(base).hostname)) throw Error('Provide a local test server, USB serial and output path');
  const server = await fetch(base+'/setup/api/flatpanel').then(r=>r.json());
  if (server.connected) throw Error('Use an idle panel on a dedicated test server');
  const browser = await chromium.launch({channel:'chrome',headless:true});
  const page = await browser.newPage({viewport:{width:1280,height:960},deviceScaleFactor:1});
  const errors = [];
  page.on('pageerror',e=>errors.push(e.message));
  let connected = false, initial;
  try {
    await page.goto(base+'/setup/v1/covercalibrator/0/setup');
    await page.getByText('Disconnected',{exact:true}).first().waitFor();
    await page.locator('#scan').click();
    await page.locator('#device option').filter({hasText:serial}).waitFor({state:'attached'});
    await Promise.all([page.waitForResponse(r=>r.url().endsWith('/setup/api/flatpanel')&&r.request().method()==='POST'),page.locator('#device').selectOption(serial)]);
    await page.getByRole('button',{name:'Connect for setup',exact:true}).click();
    connected = true;
    await page.getByText('Connected for setup · USB serial',{exact:true}).waitFor();
    await page.locator('#identity').filter({hasText:'Board=DeepSkyDad.FP2'}).waitFor();
    // Read the same production UI session; do not create a competing controller.
    initial = await page.evaluate(async()=>JSON.parse(await alpaca('action',{Action:'Regain.Status',Parameters:''})));
    if (initial.cover === 'moving') throw Error('Panel is moving');
    await page.locator('#brightness').fill('4097');
    await page.locator('#on').click();
    await page.getByText('Enter a whole brightness from 0 to 4096.',{exact:true}).waitFor();
    await page.locator('#brightness').fill('128');
    await page.locator('#on').click();
    await page.getByText('On · brightness 128 / 4096',{exact:true}).waitFor();
    await page.waitForFunction(()=>!document.getElementById('on').disabled);
    if (await page.locator('#simulation').isVisible() !== server.simulation) throw Error('Incorrect simulation badge');
    if (errors.length) throw Error(errors.join('\n'));
    await fs.mkdir(path.dirname(path.resolve(output)),{recursive:true});
    await page.screenshot({path:path.resolve(output),fullPage:true});
    console.log('Captured '+(server.simulation?'simulated':'physical')+' OFP2 setup: '+output);
  } finally {
    try {
      if (connected) {
        if (initial?.calibrator_on) {
          await page.locator('#brightness').fill(String(initial.brightness));
          await page.locator('#on').click();
          await page.getByText(`On · brightness ${initial.brightness} / 4096`,{exact:true}).waitFor();
        } else if (initial) {
          await page.locator('#off').click();
          await page.getByText('Off',{exact:true}).waitFor();
        }
        await page.getByRole('button',{name:'Disconnect setup',exact:true}).click();
        await page.getByRole('button',{name:'Connect for setup',exact:true}).waitFor();
      }
    } finally { await browser.close(); }
  }
})().catch(e=>{console.error(e);process.exitCode=1;});
