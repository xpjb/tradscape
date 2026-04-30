import fs from 'node:fs';
import { PNG } from 'pngjs';
const png = PNG.sync.read(fs.readFileSync('assets/source_sheet.png'));
const { width: W, height: H, data } = png;
console.log(`size: ${W}x${H}`);
// corner samples
for (const [x, y] of [[0,0],[W-1,0],[0,H-1],[W-1,H-1],[10,10]]) {
  const i = (y * W + x) * 4;
  console.log(`(${x},${y}) rgba=${data[i]},${data[i+1]},${data[i+2]},${data[i+3]}`);
}
// most common color in 50x50 corners
const counts = new Map();
for (let y = 0; y < 50; y++) for (let x = 0; x < 50; x++) {
  const i = (y * W + x) * 4;
  const k = `${data[i]},${data[i+1]},${data[i+2]},${data[i+3]}`;
  counts.set(k, (counts.get(k) || 0) + 1);
}
console.log('top-left 50x50 colors:');
[...counts.entries()].sort((a,b)=>b[1]-a[1]).slice(0,5).forEach(([k,v])=>console.log(`  ${k}: ${v}`));
