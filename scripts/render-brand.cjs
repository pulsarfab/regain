// Reproducible raster exports of the source SVG; requires sharp.
const sharp = require('sharp');
const fs = require('node:fs/promises');
const path = require('node:path');
(async () => {
  const root = path.resolve(__dirname, '..');
  const svg = path.join(root, 'assets/regain.svg');
  const png = await sharp(svg).resize(256,256).png().toBuffer();
  for (const file of ['assets/regain.png','src/Regain.NINA/Assets/regain.png','crates/regain-alpaca/web/regain.png'])
    await fs.writeFile(path.join(root,file),png);
  const sizes=[16,32,48,64,128,256];
  const frames = await Promise.all(sizes.map(size=>sharp(svg).resize(size,size).png().toBuffer()));
  const header = Buffer.alloc(6+16*frames.length); header.writeUInt16LE(1,2); header.writeUInt16LE(frames.length,4);
  let offset=header.length;
  frames.forEach((frame,i)=>{ const at=6+16*i,size=sizes[i]; header[at]=size%256;header[at+1]=size%256;header.writeUInt16LE(1,at+4);header.writeUInt16LE(32,at+6);header.writeUInt32LE(frame.length,at+8);header.writeUInt32LE(offset,at+12);offset+=frame.length; });
  await fs.writeFile(path.join(root,'assets/regain.ico'),Buffer.concat([header,...frames]));
})();
