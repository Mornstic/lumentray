import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { connectWhere } from '../../../../tools/cdp.mjs';

const outDir = process.argv[2];
if (!outDir) throw new Error('usage: rasterize-icons.mjs <outdir>');
mkdirSync(outDir, { recursive: true });

const client = await connectWhere((p) => p.url.includes('localhost:1420'));

async function render(svgPath, sizes, prefix) {
  const svg = readFileSync(svgPath, 'utf8');
  for (const size of sizes) {
    const dataUrl = await client.evaluate(`(async () => {
      const svg = ${JSON.stringify(svg)};
      const img = new Image();
      img.src = 'data:image/svg+xml;base64,' + btoa(unescape(encodeURIComponent(svg)));
      await img.decode();
      const c = document.createElement('canvas');
      c.width = ${size}; c.height = ${size};
      const g = c.getContext('2d');
      g.imageSmoothingEnabled = true;
      g.imageSmoothingQuality = 'high';
      g.drawImage(img, 0, 0, ${size}, ${size});
      return c.toDataURL('image/png');
    })()`);
    const buf = Buffer.from(dataUrl.split(',')[1], 'base64');
    writeFileSync(`${outDir}/${prefix}-${size}.png`, buf);
    console.log(prefix, size, buf.length);
  }
}

const here = new URL('.', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1');
await render(`${here}app-icon.svg`, [1024], 'app');
await render(`${here}tray-light.svg`, [16, 20, 24, 32, 40, 48], 'tray-light');
await render(`${here}tray-dark.svg`, [16, 20, 24, 32, 40, 48], 'tray-dark');
client.close();
