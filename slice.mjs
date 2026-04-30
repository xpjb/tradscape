import fs from 'node:fs';
import path from 'node:path';
import { PNG } from 'pngjs';

const SRC = 'assets/source_sheet.png';
const ALPHA_THRESHOLD = 16;
const MIN_AREA = 200;
const MIN_HEIGHT = 16;
const ROW_TOLERANCE = 60;

const NAMES = [
  ['tiles/grass', 'tiles/water', 'tiles/dirt', 'tiles/sand', 'tiles/stone', 'tiles/path'],
  ['entities/tree', 'entities/tree_stump', 'entities/rock', 'entities/rock_depleted', 'entities/berry_bush', 'entities/berry_bush_empty'],
  ['entities/trader', 'entities/goblin', 'entities/player', 'entities/boulder'],
  ['items/logs', 'items/berries', 'items/axe', 'items/pickaxe', 'items/coins'],
];

const png = PNG.sync.read(fs.readFileSync(SRC));
const { width: W, height: H, data } = png;
console.log(`source: ${W}x${H}`);

const filled = new Uint8Array(W * H);
for (let i = 0; i < W * H; i++) {
  filled[i] = data[i * 4 + 3] >= ALPHA_THRESHOLD ? 1 : 0;
}

const labels = new Int32Array(W * H);
const boxes = [];
let nextLabel = 1;
const stack = [];
for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    const i = y * W + x;
    if (!filled[i] || labels[i]) continue;
    nextLabel++;
    let x0 = x, y0 = y, x1 = x, y1 = y, area = 0;
    stack.push(i);
    labels[i] = nextLabel;
    while (stack.length) {
      const j = stack.pop();
      const jx = j % W, jy = (j / W) | 0;
      area++;
      if (jx < x0) x0 = jx; if (jx > x1) x1 = jx;
      if (jy < y0) y0 = jy; if (jy > y1) y1 = jy;
      if (jx > 0)     { const n = j - 1; if (filled[n] && !labels[n]) { labels[n] = nextLabel; stack.push(n); } }
      if (jx < W - 1) { const n = j + 1; if (filled[n] && !labels[n]) { labels[n] = nextLabel; stack.push(n); } }
      if (jy > 0)     { const n = j - W; if (filled[n] && !labels[n]) { labels[n] = nextLabel; stack.push(n); } }
      if (jy < H - 1) { const n = j + W; if (filled[n] && !labels[n]) { labels[n] = nextLabel; stack.push(n); } }
    }
    boxes.push({ x0, y0, x1, y1, area });
  }
}
console.log(`raw components: ${boxes.length}`);

// Merge boxes whose bounding rects overlap or nearly touch (handles sprites with detached parts).
const MERGE_GAP = 4;
function rectsClose(a, b) {
  return !(a.x1 + MERGE_GAP < b.x0 || b.x1 + MERGE_GAP < a.x0 ||
           a.y1 + MERGE_GAP < b.y0 || b.y1 + MERGE_GAP < a.y0);
}
let merged = boxes.slice();
let changed = true;
while (changed) {
  changed = false;
  outer: for (let i = 0; i < merged.length; i++) {
    for (let j = i + 1; j < merged.length; j++) {
      if (rectsClose(merged[i], merged[j])) {
        const a = merged[i], b = merged[j];
        merged[i] = {
          x0: Math.min(a.x0, b.x0), y0: Math.min(a.y0, b.y0),
          x1: Math.max(a.x1, b.x1), y1: Math.max(a.y1, b.y1),
          area: a.area + b.area,
        };
        merged.splice(j, 1);
        changed = true;
        break outer;
      }
    }
  }
}
console.log(`after merging adjacent: ${merged.length}`);

const big = merged.filter(b => b.area >= MIN_AREA && (b.y1 - b.y0) >= MIN_HEIGHT);
console.log(`after size filter: ${big.length}`);

big.sort((a, b) => (a.y0 + a.y1) - (b.y0 + b.y1));
const rows = [];
for (const b of big) {
  const cy = (b.y0 + b.y1) / 2;
  let row = rows.find(r => Math.abs(r.cy - cy) < ROW_TOLERANCE);
  if (!row) { row = { cy, items: [] }; rows.push(row); }
  row.items.push(b);
  row.cy = row.items.reduce((s, x) => s + (x.y0 + x.y1) / 2, 0) / row.items.length;
}
rows.sort((a, b) => a.cy - b.cy);
for (const r of rows) r.items.sort((a, b) => a.x0 - b.x0);

console.log('detected rows:');
rows.forEach((r, i) => console.log(`  row ${i}: ${r.items.length} sprites @ cy=${r.cy.toFixed(0)}`));

const expectedCounts = NAMES.map(r => r.length);
const actualCounts = rows.map(r => r.items.length);
if (rows.length !== NAMES.length || !expectedCounts.every((c, i) => c === actualCounts[i])) {
  console.error(`MISMATCH: expected rows ${JSON.stringify(expectedCounts)}, got ${JSON.stringify(actualCounts)}`);
  rows.forEach((r, i) => console.error(`  row ${i}:\n    ${r.items.map(b => `(${b.x0},${b.y0})-(${b.x1},${b.y1}) area=${b.area}`).join('\n    ')}`));
  process.exit(1);
}

const sprites = {};
for (let ri = 0; ri < rows.length; ri++) {
  for (let ci = 0; ci < rows[ri].items.length; ci++) {
    const b = rows[ri].items[ci];
    const name = NAMES[ri][ci];
    const w = b.x1 - b.x0 + 1;
    const h = b.y1 - b.y0 + 1;
    const out = new PNG({ width: w, height: h });
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const sIdx = ((b.y0 + y) * W + (b.x0 + x)) * 4;
        const dIdx = (y * w + x) * 4;
        out.data[dIdx]     = data[sIdx];
        out.data[dIdx + 1] = data[sIdx + 1];
        out.data[dIdx + 2] = data[sIdx + 2];
        out.data[dIdx + 3] = data[sIdx + 3];
      }
    }
    const outPath = `assets/${name}.png`;
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, PNG.sync.write(out));
    sprites[name] = { x: b.x0, y: b.y0, w, h };
    console.log(`  ${name}.png  ${w}x${h} @ (${b.x0},${b.y0})`);
  }
}

fs.writeFileSync('assets/sprites.json', JSON.stringify({ sheet: 'source_sheet.png', sprites }, null, 2));
console.log('wrote assets/sprites.json');
