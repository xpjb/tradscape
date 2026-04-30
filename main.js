const TILE = 48;
const VIEW_W = 15, VIEW_H = 15;

const canvas = document.getElementById('world');
const ctx = canvas.getContext('2d');
canvas.width = VIEW_W * TILE;
canvas.height = VIEW_H * TILE;

let mapInfo = null;
let state = null;
let tickMs = 200;
let lastTickAt = performance.now();

// id -> { px, py, prevX, prevY, fromTime }
const entityAnim = new Map();
// active transient effects: { kind, x, y, start, dur, dmg? }
const effects = [];
const seenChatIds = new Set();

const TILE_IMG = {
  grass: 'tiles/grass.png', dirt: 'tiles/dirt.png', sand: 'tiles/sand.png',
  water: 'tiles/water.png', stone: 'tiles/stone.png', path: 'tiles/path.png',
};
const OBJ_IMG = {
  tree: 'entities/tree.png', tree_tier_1: 'entities/tree_pine.png', tree_tier_2: 'entities/tree_oak.png', tree_tier_3: 'entities/tree_yew.png',
  stump: 'entities/tree_stump.png',
  rock: 'entities/rock.png', rock_tier_1: 'entities/rock_copper.png', rock_tier_2: 'entities/rock_iron.png', rock_tier_3: 'entities/rock_gold.png',
  depleted_rock: 'entities/rock_depleted.png',
  bush: 'entities/berry_bush.png', bush_empty: 'entities/berry_bush_empty.png',
  boulder: 'entities/boulder.png', trader: 'entities/trader.png',
};
const MOB_IMG = {
  goblin: 'entities/goblin.png',
  club_goblin: 'entities/club_goblin.png',
  ninja: 'entities/ninja.png',
};
const ITEM_IMG = {
  pine_logs: 'items/pine_logs.png', oak_logs: 'items/oak_logs.png', yew_logs: 'items/yew_logs.png',
  copper_ore: 'items/copper_ore.png', iron_ore: 'items/iron_ore.png', gold_ore: 'items/gold_ore.png',
  berries: 'items/berries.png', coins: 'items/coins.png',
  bronze_axe: 'items/bronze_axe.png', iron_axe: 'items/iron_axe.png', steel_axe: 'items/steel_axe.png',
  bronze_pickaxe: 'items/bronze_pickaxe.png', iron_pickaxe: 'items/iron_pickaxe.png', steel_pickaxe: 'items/steel_pickaxe.png',
};
const TILE_COLOR = { grass: '#3a7d2c', dirt: '#7a5a3b', sand: '#d9c787', water: '#2a5fb0', stone: '#888', path: '#a99a82' };
const OBJ_COLOR  = { tree: '#1f5417', stump: '#5a3a1f', rock: '#666', depleted_rock: '#aaa', bush: '#7a3', boulder: '#777', trader: '#c0a060' };
const MOB_COLOR  = { goblin: '#7caa3c', club_goblin: '#6b8f2a', ninja: '#2b2b35' };
const MOB_LABEL = { goblin: 'G', club_goblin: 'C', ninja: 'N' };
const WALKABLE_OBJ = new Set(['none']);
const ITEM_NAME = {
  pine_logs: 'Pine logs', oak_logs: 'Oak logs', yew_logs: 'Yew logs',
  copper_ore: 'Copper ore', iron_ore: 'Iron ore', gold_ore: 'Gold ore',
  berries: 'Berries', coins: 'Coins',
  bronze_axe: 'Bronze axe', iron_axe: 'Iron axe', steel_axe: 'Steel axe',
  bronze_pickaxe: 'Bronze pickaxe', iron_pickaxe: 'Iron pickaxe', steel_pickaxe: 'Steel pickaxe',
};

function itemName(item) {
  return ITEM_NAME[item] || item.replaceAll('_', ' ');
}

function itemIcon(item) {
  return ITEM_IMG[item] || `items/${item}.png`;
}

function objectArtKey(o) {
  if (o.kind === 'bush' && o.berries === 0) return 'bush_empty';
  if ((o.kind === 'tree' || o.kind === 'rock') && o.tier) return `${o.kind}_tier_${o.tier}`;
  return o.kind;
}

function objectLabel(o) {
  if ((o.kind === 'tree' || o.kind === 'rock') && o.tier) return `${o.kind[0].toUpperCase()}${o.tier}`;
  return o.kind[0].toUpperCase();
}

const images = {};
function img(name) {
  if (!name) return null;
  if (!images[name]) {
    const i = new Image();
    i.src = `assets/${name}`;
    i.onerror = () => { i.failed = true; };
    images[name] = i;
  }
  return images[name];
}

function drawCell(im, fallback, label, px, py) {
  if (im && im.complete && im.naturalWidth > 0 && !im.failed) {
    ctx.drawImage(im, px, py, TILE, TILE);
  } else {
    ctx.fillStyle = fallback;
    ctx.fillRect(px, py, TILE, TILE);
    if (label) {
      ctx.fillStyle = '#000';
      ctx.font = '10px monospace';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(label, px + TILE/2, py + TILE/2);
    }
  }
}

/** Directory URL for app root (fixes /tradscape vs /tradscape/ vs …/index.html). */
function tradscapeBaseHref() {
  const p = window.location.pathname;
  if (p.endsWith('/')) return window.location.origin + p;
  const leaf = (p.split('/').pop()) || '';
  if (/\.[a-z0-9]+$/i.test(leaf)) {
    const d = p.slice(0, p.lastIndexOf('/') + 1);
    return window.location.origin + d;
  }
  return window.location.origin + p + '/';
}

/** WebSocket at same path tier as the app (e.g. …/tradscape/ws behind a subpath proxy). */
function tradscapeWsUrl() {
  const u = new URL('./ws', tradscapeBaseHref());
  u.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return u.href;
}
const ws = new WebSocket(tradscapeWsUrl());
ws.onopen = () => {
  const uuid = localStorage.getItem('tradscape_uuid') || '';
  const name = (localStorage.getItem('tradscape_name')) || prompt('Character name?', 'Adventurer') || 'Adventurer';
  localStorage.setItem('tradscape_name', name);
  ws.send(JSON.stringify({ t: 'join', uuid, name }));
};
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.t === 'init') {
    mapInfo = m;
    if (m.uuid) localStorage.setItem('tradscape_uuid', m.uuid);
  } else if (m.t === 'state') {
    if (m.tick_ms) tickMs = m.tick_ms;
    onState(m);
  }
};
ws.onclose = () => addLog('[disconnected]');

function entKey(kind, id) { return `${kind}:${id}`; }

function onState(m) {
  const now = performance.now();
  lastTickAt = now;
  const seen = new Set();
  const upsert = (k, x, y) => {
    seen.add(k);
    const prev = entityAnim.get(k);
    if (!prev) {
      entityAnim.set(k, { px: x, py: y, prevX: x, prevY: y, fromTime: now });
    } else if (prev.px !== x || prev.py !== y) {
      // start a new interpolation from current rendered position
      const t = Math.min(1, (now - prev.fromTime) / tickMs);
      const curX = prev.prevX + (prev.px - prev.prevX) * t;
      const curY = prev.prevY + (prev.py - prev.prevY) * t;
      entityAnim.set(k, { px: x, py: y, prevX: curX, prevY: curY, fromTime: now });
    }
  };
  for (const p of m.players) upsert(entKey('p', p.id), p.x, p.y);
  for (const mob of m.mobs) upsert(entKey('m', mob.id), mob.x, mob.y);
  // prune entities that disappeared
  for (const k of [...entityAnim.keys()]) if (!seen.has(k)) entityAnim.delete(k);

  if (m.events) {
    for (const e of m.events) pushEffect(e, now);
  }
  if (m.log && m.log.length) for (const l of m.log) addLog(l);

  state = m;
  updatePanel(m);
}

function pushEffect(e, now) {
  const dur = {
    chop: 320, mine: 320, pick: 280,
    hit_mob: 600, hit_player: 600, miss_mob: 500, miss_player: 500,
  }[e.k] || 300;
  effects.push({ kind: e.k, x: e.x, y: e.y, dmg: e.dmg, start: now, dur });
}

function camera() {
  if (!state) return [0, 0];
  const me = entityAnim.get(entKey('p', state.you.id));
  let yx = state.you.x, yy = state.you.y;
  if (me) {
    const t = Math.min(1, (performance.now() - me.fromTime) / tickMs);
    yx = me.prevX + (me.px - me.prevX) * t;
    yy = me.prevY + (me.py - me.prevY) * t;
  }
  let cx = yx - Math.floor(VIEW_W / 2);
  let cy = yy - Math.floor(VIEW_H / 2);
  if (mapInfo) {
    cx = Math.max(0, Math.min(mapInfo.w - VIEW_W, cx));
    cy = Math.max(0, Math.min(mapInfo.h - VIEW_H, cy));
  }
  return [cx, cy];
}

function entPos(kind, id, fallbackX, fallbackY) {
  const a = entityAnim.get(entKey(kind, id));
  if (!a) return [fallbackX, fallbackY];
  const t = Math.min(1, (performance.now() - a.fromTime) / tickMs);
  return [a.prevX + (a.px - a.prevX) * t, a.prevY + (a.py - a.prevY) * t];
}

function render() {
  if (!state || !mapInfo) return;
  const now = performance.now();
  const [cx, cy] = camera();
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  const cxi = Math.floor(cx), cyi = Math.floor(cy);
  const ox = (cxi - cx) * TILE, oy = (cyi - cy) * TILE;

  // tiles + objects (snap to integer tile coords; offset whole layer for smooth camera)
  for (let vy = -1; vy <= VIEW_H; vy++) {
    for (let vx = -1; vx <= VIEW_W; vx++) {
      const x = cxi + vx, y = cyi + vy;
      if (x < 0 || y < 0 || x >= mapInfo.w || y >= mapInfo.h) continue;
      const px = vx * TILE + ox, py = vy * TILE + oy;
      const t = mapInfo.tiles[y * mapInfo.w + x];
      drawCell(img(TILE_IMG[t]), TILE_COLOR[t] || '#444', t[0], px, py);
      const o = state.objects[y * mapInfo.w + x];
      if (o.kind && o.kind !== 'none') {
        const artKey = objectArtKey(o);
        drawCell(img(OBJ_IMG[artKey]), OBJ_COLOR[o.kind] || '#888', objectLabel(o), px, py);
      }
    }
  }

  // path + target (prediction)
  if (state.you.target) {
    const [tx, ty] = state.you.target;
    const [yx, yy] = entPos('p', state.you.id, state.you.x, state.you.y);
    const intent = (state.you.intent && state.you.intent.k) || 'none';
    const path = predictPath([state.you.x, state.you.y], [tx, ty], intent !== 'none');
    drawPath(yx, yy, path, cx, cy, now);
    drawTarget(tx, ty, cx, cy, now);
  }

  // mobs
  for (const m of state.mobs) {
    const [ex, ey] = entPos('m', m.id, m.x, m.y);
    const vx = ex - cx, vy = ey - cy;
    if (vx < -1 || vy < -1 || vx > VIEW_W || vy > VIEW_H) continue;
    drawCell(img(MOB_IMG[m.kind] || ''), MOB_COLOR[m.kind] || '#a33', MOB_LABEL[m.kind] || 'M', vx * TILE, vy * TILE);
    drawHpBar(vx * TILE, vy * TILE, m.hp / m.hp_max);
  }

  // players
  for (const p of state.players) {
    const [ex, ey] = entPos('p', p.id, p.x, p.y);
    const vx = ex - cx, vy = ey - cy;
    if (vx < -1 || vy < -1 || vx > VIEW_W || vy > VIEW_H) continue;
    drawCell(img('entities/player.png'), '#e8d9b8', 'P', vx * TILE, vy * TILE);
    if (p.id === state.you.id) {
      ctx.strokeStyle = '#ffe066'; ctx.lineWidth = 2;
      ctx.strokeRect(vx * TILE + 1, vy * TILE + 1, TILE - 2, TILE - 2);
    }
    ctx.fillStyle = '#fff';
    ctx.font = 'bold 11px monospace';
    ctx.textAlign = 'center';
    ctx.fillText(p.name, vx * TILE + TILE / 2, vy * TILE - 2);
    drawHpBar(vx * TILE, vy * TILE, p.hp / p.hp_max);
  }

  // effects on top
  for (let i = effects.length - 1; i >= 0; i--) {
    const e = effects[i];
    const t = (now - e.start) / e.dur;
    if (t >= 1) { effects.splice(i, 1); continue; }
    drawEffect(e, t, cx, cy);
  }
}

function drawTarget(wx, wy, cx, cy, now) {
  const vx = wx - cx, vy = wy - cy;
  const px = vx * TILE, py = vy * TILE;
  const pulse = 0.5 + 0.5 * Math.sin(now / 120);
  ctx.save();
  ctx.strokeStyle = `rgba(255, 80, 80, ${0.55 + 0.35 * pulse})`;
  ctx.lineWidth = 2;
  const inset = 2 + pulse * 3;
  ctx.strokeRect(px + inset, py + inset, TILE - inset * 2, TILE - inset * 2);
  ctx.restore();
}

/** Mirrors server walkable() for predicted routes (players + live mobs block). */
function clientWalkable(wx, wy, myId) {
  if (!mapInfo || !state) return false;
  if (wx < 0 || wy < 0 || wx >= mapInfo.w || wy >= mapInfo.h) return false;
  const t = mapInfo.tiles[wy * mapInfo.w + wx];
  if (t === 'water') return false;
  const o = state.objects[wy * mapInfo.w + wx];
  if (!WALKABLE_OBJ.has(o.kind)) return false;
  for (const p of state.players) {
    if (p.id !== myId && p.x === wx && p.y === wy) return false;
  }
  for (const m of state.mobs) {
    if (m.x === wx && m.y === wy) return false;
  }
  return true;
}

/**
 * Client-side BFS matching server: 8-neigh, corner-cut rule, Step vs Adjacent goal.
 * Returns world tile centers to visit in order (empty if already at goal or unreachable).
 */
function predictPath(from, goal, needsAdjacent) {
  if (!mapInfo || !state) return [];
  const [fx, fy] = from;
  const [gx, gy] = goal;
  const myId = state.you.id;
  const goalKey = `${gx},${gy}`;
  const cheb = (x, y) => Math.max(Math.abs(x - gx), Math.abs(y - gy));
  const atGoal = needsAdjacent ? cheb(fx, fy) === 1 : fx === gx && fy === gy;
  if (atGoal) return [];

  const key = (x, y) => `${x},${y}`;
  const prev = new Map();
  const q = [[fx, fy]];
  prev.set(key(fx, fy), [fx, fy]);
  const dirs = [[1, 0], [-1, 0], [0, 1], [0, -1], [1, 1], [1, -1], [-1, 1], [-1, -1]];

  while (q.length) {
    const [cx, cy] = q.shift();
    for (const [dx, dy] of dirs) {
      const nx = cx + dx, ny = cy + dy;
      const nk = key(nx, ny);
      if (prev.has(nk)) continue;
      if (needsAdjacent && nk === goalKey) continue;
      if (!clientWalkable(nx, ny, myId)) continue;
      if (dx !== 0 && dy !== 0) {
        if (!clientWalkable(cx + dx, cy, myId)) continue;
        if (!clientWalkable(cx, cy + dy, myId)) continue;
      }
      prev.set(nk, [cx, cy]);
      const reached = needsAdjacent ? cheb(nx, ny) === 1 : nx === gx && ny === gy;
      if (reached) {
        const out = [];
        let c = [nx, ny];
        while (c[0] !== fx || c[1] !== fy) {
          out.unshift(c);
          c = prev.get(key(c[0], c[1]));
        }
        return out;
      }
      q.push([nx, ny]);
    }
  }
  return [];
}

/** pathTiles: array of [x,y] world steps; startX/startY interpolated player (world). */
function drawPath(startX, startY, pathTiles, cx, cy, now) {
  if (!pathTiles.length) return;
  ctx.save();
  ctx.strokeStyle = 'rgba(255, 224, 102, 0.55)';
  ctx.lineWidth = 2;
  ctx.setLineDash([6, 5]);
  ctx.lineDashOffset = -now / 35;
  ctx.beginPath();
  ctx.moveTo((startX - cx) * TILE + TILE / 2, (startY - cy) * TILE + TILE / 2);
  for (const [tx, ty] of pathTiles) {
    ctx.lineTo((tx - cx) * TILE + TILE / 2, (ty - cy) * TILE + TILE / 2);
  }
  ctx.stroke();
  ctx.setLineDash([]);
  ctx.fillStyle = 'rgba(255, 224, 102, 0.75)';
  for (const [tx, ty] of pathTiles) {
    ctx.beginPath();
    ctx.arc((tx - cx) * TILE + TILE / 2, (ty - cy) * TILE + TILE / 2, 3, 0, Math.PI * 2);
    ctx.fill();
  }
  ctx.restore();
}

function drawEffect(e, t, cx, cy) {
  const vx = e.x - cx, vy = e.y - cy;
  if (vx < -1 || vy < -1 || vx > VIEW_W || vy > VIEW_H) return;
  const px = vx * TILE + TILE / 2;
  const py = vy * TILE + TILE / 2;
  ctx.save();
  if (e.kind === 'chop') {
    const r = TILE * 0.15 + t * TILE * 0.5;
    ctx.strokeStyle = `rgba(220, 180, 90, ${1 - t})`;
    ctx.lineWidth = 3;
    ctx.beginPath(); ctx.arc(px, py, r, 0, Math.PI * 2); ctx.stroke();
    // chip flecks
    ctx.fillStyle = `rgba(180, 130, 60, ${1 - t})`;
    for (let i = 0; i < 4; i++) {
      const ang = (i / 4) * Math.PI * 2 + t * 4;
      const d = 6 + t * 16;
      ctx.fillRect(px + Math.cos(ang) * d - 2, py + Math.sin(ang) * d - 2, 4, 4);
    }
  } else if (e.kind === 'mine') {
    ctx.fillStyle = `rgba(255, 220, 120, ${1 - t})`;
    for (let i = 0; i < 6; i++) {
      const ang = (i / 6) * Math.PI * 2 + t * 6;
      const d = 4 + t * 18;
      ctx.beginPath();
      ctx.arc(px + Math.cos(ang) * d, py + Math.sin(ang) * d, 2 + (1 - t) * 2, 0, Math.PI * 2);
      ctx.fill();
    }
  } else if (e.kind === 'pick') {
    ctx.fillStyle = `rgba(120, 220, 110, ${1 - t})`;
    for (let i = 0; i < 5; i++) {
      const ang = (i / 5) * Math.PI * 2;
      const d = t * 14;
      ctx.beginPath();
      ctx.arc(px + Math.cos(ang) * d, py + Math.sin(ang) * d - t * 8, 3 * (1 - t * 0.5), 0, Math.PI * 2);
      ctx.fill();
    }
  } else if (e.kind === 'hit_mob' || e.kind === 'hit_player') {
    // red flash on tile
    ctx.fillStyle = `rgba(255, 40, 40, ${0.4 * (1 - t)})`;
    ctx.fillRect(vx * TILE, vy * TILE, TILE, TILE);
    // damage number floats up
    const fy = py - t * 28;
    ctx.fillStyle = `rgba(255, 80, 80, ${1 - t})`;
    ctx.font = 'bold 18px monospace';
    ctx.textAlign = 'center';
    ctx.lineWidth = 3;
    ctx.strokeStyle = `rgba(0, 0, 0, ${1 - t})`;
    const txt = String(e.dmg);
    ctx.strokeText(txt, px, fy);
    ctx.fillText(txt, px, fy);
  } else if (e.kind === 'miss_mob' || e.kind === 'miss_player') {
    const fy = py - t * 22;
    ctx.fillStyle = `rgba(220, 220, 220, ${1 - t})`;
    ctx.font = 'bold 14px monospace';
    ctx.textAlign = 'center';
    ctx.strokeStyle = `rgba(0, 0, 0, ${1 - t})`;
    ctx.lineWidth = 3;
    ctx.strokeText('miss', px, fy);
    ctx.fillText('miss', px, fy);
  }
  ctx.restore();
}

function drawHpBar(px, py, frac) {
  ctx.fillStyle = '#400';
  ctx.fillRect(px + 4, py + 2, TILE - 8, 4);
  ctx.fillStyle = '#0c0';
  ctx.fillRect(px + 4, py + 2, (TILE - 8) * Math.max(0, Math.min(1, frac)), 4);
}

function rafLoop() {
  render();
  requestAnimationFrame(rafLoop);
}
requestAnimationFrame(rafLoop);

canvas.addEventListener('click', (e) => {
  if (!state || !mapInfo) return;
  const r = canvas.getBoundingClientRect();
  const [cx, cy] = camera();
  const mx = e.clientX - r.left;
  const my = e.clientY - r.top;
  const clickedMob = mobAtScreenPoint(mx, my, cx, cy);
  if (clickedMob) {
    ws.send(JSON.stringify({ t: 'attack', mid: clickedMob.id }));
    return;
  }
  const wx = Math.floor(cx + mx / TILE);
  const wy = Math.floor(cy + my / TILE);
  ws.send(JSON.stringify({ t: 'click', x: wx, y: wy }));
});
canvas.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  ws.send(JSON.stringify({ t: 'stop' }));
});

document.querySelectorAll('#tabs button').forEach(b => {
  b.onclick = () => {
    document.querySelectorAll('#tabs button').forEach(x => x.classList.remove('active'));
    document.querySelectorAll('.tab-content').forEach(x => x.classList.add('hidden'));
    b.classList.add('active');
    document.getElementById('tab-' + b.dataset.tab).classList.remove('hidden');
  };
});

function mobAtScreenPoint(mx, my, cx, cy) {
  for (let i = state.mobs.length - 1; i >= 0; i--) {
    const m = state.mobs[i];
    const [ex, ey] = entPos('m', m.id, m.x, m.y);
    const px = (ex - cx) * TILE;
    const py = (ey - cy) * TILE;
    if (mx >= px && mx < px + TILE && my >= py && my < py + TILE) {
      return m;
    }
  }
  return null;
}

document.getElementById('trade-close').onclick = () => {
  document.getElementById('trade-window').classList.add('hidden');
  ws.send(JSON.stringify({ t: 'close_trade' }));
};

document.getElementById('chat-form').onsubmit = (e) => {
  e.preventDefault();
  const input = document.getElementById('chat-input');
  const text = input.value.trim();
  if (!text) return;
  ws.send(JSON.stringify({ t: 'chat', text }));
  input.value = '';
};

function updatePanel(s) {
  const grid = document.getElementById('inv-grid');
  grid.innerHTML = '';
  for (let i = 0; i < 28; i++) {
    const slot = document.createElement('div');
    slot.className = 'inv-slot';
    const it = s.you.inv[i];
    if (it && it.item) {
      slot.classList.add('has-item');
      const im = document.createElement('img');
      const src = ITEM_IMG[it.item] || `items/${it.item}.png`;
      im.src = `assets/${src}`;
      im.onerror = () => { im.style.display = 'none'; };
      slot.appendChild(im);
      const lbl = document.createElement('span');
      lbl.className = 'label';
      lbl.textContent = itemName(it.item);
      slot.appendChild(lbl);
      if (it.qty > 1) {
        const q = document.createElement('span');
        q.className = 'qty';
        q.textContent = it.qty > 9999 ? Math.floor(it.qty/1000) + 'k' : it.qty;
        slot.appendChild(q);
      }
      slot.title = `${itemName(it.item)} x${it.qty}`;
      slot.onclick = () => {
        if (it.item === 'berries') ws.send(JSON.stringify({ t: 'eat', slot: i }));
      };
      slot.oncontextmenu = (e) => {
        e.preventDefault();
        ws.send(JSON.stringify({ t: 'sell', slot: i }));
      };
    }
    grid.appendChild(slot);
  }
  document.getElementById('eq-list').textContent = `axe T${s.you.axe_tier || 0}, pickaxe T${s.you.pickaxe_tier || 0}`;
  renderTrade(s);
  renderChat(s.chat || []);

  const sk = s.you.skills;
  document.getElementById('skills-list').innerHTML = `
    <div>Woodcutting: ${sk.woodcutting} <span style="color:#aaa">(${sk.woodcutting_xp} xp)</span></div>
    <div>Mining: ${sk.mining} <span style="color:#aaa">(${sk.mining_xp} xp)</span></div>
    <div>Attack: ${sk.attack} <span style="color:#aaa">(${sk.attack_xp} xp)</span></div>
    <div>Strength: ${sk.strength} <span style="color:#aaa">(${sk.strength_xp} xp)</span></div>
    <div>Defence: ${sk.defence} <span style="color:#aaa">(${sk.defence_xp} xp)</span></div>
    <div>HP: ${sk.hp} <span style="color:#aaa">(${sk.hp_xp} xp)</span></div>`;
  document.getElementById('hp-cur').textContent = s.you.hp;
  document.getElementById('hp-max').textContent = sk.hp;
}

function renderTrade(s) {
  const win = document.getElementById('trade-window');
  const buyList = document.getElementById('shop-buy-list');
  const sellList = document.getElementById('shop-sell-list');
  if (!win || !buyList || !sellList) return;

  win.classList.toggle('hidden', !s.you.trade_open);

  buyList.innerHTML = '';
  for (const row of s.shop || []) {
    buyList.appendChild(tradeButton({
      item: row.item,
      name: row.name,
      detail: `T${row.tier} ${row.buy}gp`,
      onClick: () => ws.send(JSON.stringify({ t: 'buy', item: row.item })),
    }));
  }

  sellList.innerHTML = '';
  let anyItem = false;
  for (let i = 0; i < s.you.inv.length; i++) {
    const it = s.you.inv[i];
    if (!it || !it.item) continue;
    anyItem = true;
    const sale = (s.sells || []).find(x => x.item === it.item);
    const canSell = it.item !== 'coins' && sale;
    sellList.appendChild(tradeButton({
      item: it.item,
      name: `${itemName(it.item)} x${it.qty}`,
      detail: canSell ? `${sale.sell * it.qty}gp` : 'not for sale',
      onClick: () => ws.send(JSON.stringify({ t: 'sell', slot: i })),
      disabled: !canSell,
    }));
  }
  if (!anyItem) {
    const empty = document.createElement('div');
    empty.className = 'hint';
    empty.textContent = 'Your inventory is empty.';
    sellList.appendChild(empty);
  }
}

function tradeButton({ item, name, detail, onClick, disabled = false }) {
  const el = document.createElement('button');
  el.className = 'trade-row';
  el.disabled = disabled;
  const im = document.createElement('img');
  im.src = `assets/${itemIcon(item)}`;
  im.onerror = () => { im.style.display = 'none'; };
  const label = document.createElement('span');
  label.className = 'trade-name';
  label.textContent = name;
  const price = document.createElement('span');
  price.className = 'trade-price';
  price.textContent = detail;
  el.append(im, label, price);
  el.onclick = onClick;
  return el;
}

function renderChat(messages) {
  const el = document.getElementById('chat-log');
  if (!el) return;
  const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
  for (const msg of messages) {
    if (seenChatIds.has(msg.id)) continue;
    seenChatIds.add(msg.id);
    const row = document.createElement('div');
    const name = document.createElement('span');
    name.className = 'chat-name';
    name.textContent = `${msg.name}: `;
    const text = document.createElement('span');
    text.textContent = msg.text;
    row.append(name, text);
    el.appendChild(row);
  }
  trimChatLog(el);
  if (atBottom) el.scrollTop = el.scrollHeight;
}

function addLog(line) {
  const el = document.getElementById('chat-log');
  if (!el) return;
  const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
  const row = document.createElement('div');
  row.className = 'chat-system';
  const name = document.createElement('span');
  name.className = 'chat-name';
  name.textContent = 'System: ';
  const text = document.createElement('span');
  text.textContent = line;
  row.append(name, text);
  el.appendChild(row);
  trimChatLog(el);
  if (atBottom) el.scrollTop = el.scrollHeight;
}

function trimChatLog(el) {
  while (el.children.length > 100) el.removeChild(el.firstChild);
}
