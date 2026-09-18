// Capture the real setup pages against an explicitly simulated local server.
// Usage: node scripts/screenshot-alpaca.cjs http://127.0.0.1:11237 docs/images
const { chromium } = require('playwright');
const path = require('node:path');
const fs = require('node:fs/promises');

(async () => {
  const base = process.argv[2] || 'http://127.0.0.1:11237';
  const output = path.resolve(process.argv[3] || 'docs/images');
  const url = new URL(base);
  if (!['127.0.0.1', 'localhost'].includes(url.hostname)) throw Error('Use a local simulation server');
  const state = await fetch(base + '/setup/api/state').then(r => r.json());
  if (!state.simulation) throw Error('Start the server with --simulate and a temporary --profiles file');
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 960 }, deviceScaleFactor: 1 });
    await page.goto(base + '/setup');
    await page.locator('#label').waitFor();
    await page.locator('#backend').selectOption('direct');
    await page.locator('#scan').click();
    await page.getByText(/Found \d+ cameras/).waitFor();
    await page.locator('#camera-choice').selectOption({ label: 'ZWO ASI676MC' });
    await page.locator('#save').click();
    await page.getByText('Settings saved.').waitFor();
    await page.screenshot({ path: path.join(output, 'alpaca-camera.png'), fullPage: true });
    await page.locator('[data-tab="recovery"]').click();
    await page.screenshot({ path: path.join(output, 'alpaca-recovery.png'), fullPage: true });
    await page.goto(base + '/setup/v1/rotator/0/setup');
    await page.locator('#scan').click();
    await page.locator('#device option[value="0102030405060708"]').waitFor({ state: 'attached' });
    await page.locator('#device').selectOption('0102030405060708');
    await page.getByRole('button', { name: 'Connect for setup', exact: true }).click();
    await page.getByRole('button', { name: 'Disconnect setup', exact: true }).waitFor();
    await page.getByText(/Sky 152.00°/).waitFor();
    await page.screenshot({ path: path.join(output, 'alpaca-rotator.png'), fullPage: true });
    await page.getByRole('button', { name: 'Disconnect setup', exact: true }).click();
    for (const [type,serial] of [['filterwheel','0102030405060708'],['focuser','0102030405060709']]) {
      await page.goto(base + '/setup/v1/' + type + '/0/setup');
      await page.getByText('Disconnected', {exact:true}).waitFor();
      await page.locator('#scan').click();
      await page.locator('#device option[value="'+serial+'"]').waitFor({state:'attached'});
      await Promise.all([page.waitForResponse(r=>r.request().method()==='POST' && r.url().endsWith('/'+(type==='filterwheel'?'efw':'eaf'))),page.locator('#device').selectOption(serial)]);
      await page.getByRole('button',{name:'Connect for setup',exact:true}).click();
      await page.getByText('Connected for setup',{exact:true}).waitFor();
      await page.locator('#position').filter({hasText:type==='filterwheel'?'Slot 1 of 7':'Position 39829'}).waitFor();
      if(type==='filterwheel') {
        await page.locator('#calibrate').click();
        await page.locator('#position').filter({hasText:'Calibrating…'}).waitFor();
        if(await page.locator('#calibrate').isEnabled() || await page.locator('#move').isEnabled() || await page.locator('#save-filters').isEnabled()) throw Error('Motion controls remained enabled during calibration');
        await page.locator('#position').filter({hasText:'Slot 1 of 7'}).waitFor();
        if(!await page.locator('#calibrate').isEnabled()) throw Error('Calibration control did not return to idle');
        if(await page.locator('[data-filter-name]').count()!==7) throw Error('Calibration changed filter metadata');
      }
      await page.screenshot({path:path.join(output,'alpaca-'+(type==='filterwheel'?'efw':'eaf')+'.png'),fullPage:true});
      await page.getByRole('button',{name:'Disconnect setup',exact:true}).click();
      await page.getByRole('button',{name:'Connect for setup',exact:true}).waitFor();
    }
    console.log('Saved Alpaca camera, recovery, rotator, filter wheel and focuser screenshots.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
