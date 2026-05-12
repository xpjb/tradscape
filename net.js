// WebTransport + binary wire layer.
// Maintains pool mirrors and synthesizes the legacy `state` object shape so
// the existing render/UI code in main.js keeps working without changes.

const MSG_CLIENT_JOIN     = 0x01;
const MSG_SERVER_INIT     = 0x02;
const MSG_SERVER_BASELINE = 0x03;
const MSG_SERVER_DELTA    = 0x04;
const MSG_CLIENT_ACK      = 0x05;
const MSG_CLIENT_CMD      = 0x06;
const MSG_SERVER_CHAT     = 0x07;
const MSG_SERVER_EVENTS   = 0x08;
const MSG_SERVER_LOG      = 0x09;
const MSG_SERVER_CATALOGS = 0x0A;
const MSG_SERVER_YOUVIEW  = 0x0B;

const POOL_PLAYER = 0, POOL_MOB = 1, POOL_GROUND = 2, POOL_OBJECT = 3, POOL_TERMINATOR = 0xFF;
const OP_UPDATE = 0, OP_SPAWN = 1, OP_DESPAWN = 2;

const PC_X=1<<0, PC_Y=1<<1, PC_HP_CUR=1<<2, PC_HP_MAX=1<<3, PC_NAME=1<<4,
      PC_SKILLS=1<<5, PC_INV=1<<6, PC_EQUIP=1<<7, PC_INTENT=1<<8, PC_TARGET=1<<9, PC_UI=1<<10;
const MC_X=1<<0, MC_Y=1<<1, MC_HP_CUR=1<<2, MC_HP_MAX=1<<3, MC_KIND=1<<4;
const GC_X=1<<0, GC_Y=1<<1, GC_ITEM=1<<2, GC_QTY=1<<3;

const TILE_NAMES = ['grass', 'dirt', 'sand', 'water', 'stone', 'path'];
const MOB_KINDS = ['goblin', 'club_goblin', 'ninja', 'dragon'];

// ─── Codec primitives (DataView-based) ──────────────────────────────────────

class BufReader {
  constructor(buf, pos = 0) {
    this.buf = buf;        // ArrayBuffer
    this.dv = new DataView(buf);
    this.pos = pos;
  }
  u8() { const v = this.dv.getUint8(this.pos); this.pos += 1; return v; }
  u16() { const v = this.dv.getUint16(this.pos, true); this.pos += 2; return v; }
  u32() { const v = this.dv.getUint32(this.pos, true); this.pos += 4; return v; }
  i16() { const v = this.dv.getInt16(this.pos, true); this.pos += 2; return v; }
  i32() { const v = this.dv.getInt32(this.pos, true); this.pos += 4; return v; }
  bytes(n) {
    const out = new Uint8Array(this.buf, this.pos, n);
    this.pos += n;
    return out;
  }
  strU8() {
    const n = this.u8();
    const b = this.bytes(n);
    return new TextDecoder().decode(b);
  }
  strU16() {
    const n = this.u16();
    const b = this.bytes(n);
    return new TextDecoder().decode(b);
  }
}

class BufWriter {
  constructor() {
    this.chunks = [];
    this.len = 0;
  }
  u8(v) { const b = new Uint8Array([v]); this.chunks.push(b); this.len += 1; }
  u16(v) {
    const b = new ArrayBuffer(2);
    new DataView(b).setUint16(0, v, true);
    this.chunks.push(new Uint8Array(b));
    this.len += 2;
  }
  u32(v) {
    const b = new ArrayBuffer(4);
    new DataView(b).setUint32(0, v, true);
    this.chunks.push(new Uint8Array(b));
    this.len += 4;
  }
  i32(v) {
    const b = new ArrayBuffer(4);
    new DataView(b).setInt32(0, v, true);
    this.chunks.push(new Uint8Array(b));
    this.len += 4;
  }
  bytes(b) { this.chunks.push(b); this.len += b.byteLength; }
  strU8(s) {
    const enc = new TextEncoder().encode(s);
    const n = Math.min(enc.byteLength, 255);
    this.u8(n);
    this.bytes(enc.subarray(0, n));
  }
  strU16(s) {
    const enc = new TextEncoder().encode(s);
    const n = Math.min(enc.byteLength, 65535);
    this.u16(n);
    this.bytes(enc.subarray(0, n));
  }
  toBytes() {
    const out = new Uint8Array(this.len);
    let p = 0;
    for (const c of this.chunks) { out.set(c, p); p += c.byteLength; }
    return out;
  }
}

// ─── Command kinds (must match server wire.rs) ──────────────────────────────

const CMD = {
  click: 1, attack: 2, trade_player: 3, stop: 4, eat: 5, drop: 6, equip: 7,
  unequip: 8, buy: 9, sell: 10, close_trade: 11, close_forge: 12, close_angler: 13,
  angler_buy: 14, forge: 15, close_player_trade: 16, trade_offer_slot: 17,
  trade_accept: 18, trade_confirm: 19, angel_confirm: 20, angel_decline: 21, chat: 22,
};

function encodeCommand(obj) {
  const w = new BufWriter();
  w.u8(MSG_CLIENT_CMD);
  const t = obj.t;
  switch (t) {
    case 'click':      w.u8(CMD.click); w.i32(obj.x|0); w.i32(obj.y|0); break;
    case 'attack':     w.u8(CMD.attack); w.u32(obj.mid|0); break;
    case 'trade_player': w.u8(CMD.trade_player); w.u32(obj.pid|0); break;
    case 'stop':       w.u8(CMD.stop); break;
    case 'eat':        w.u8(CMD.eat); w.u8(obj.slot|0); break;
    case 'drop':       w.u8(CMD.drop); w.u8(obj.slot|0); break;
    case 'equip':      w.u8(CMD.equip); w.u8(obj.slot|0); break;
    case 'unequip':    w.u8(CMD.unequip); w.strU8(obj.slot); break;
    case 'buy':        w.u8(CMD.buy); w.strU8(obj.item); break;
    case 'sell':       w.u8(CMD.sell); w.u8(obj.slot|0); break;
    case 'close_trade': w.u8(CMD.close_trade); break;
    case 'close_forge': w.u8(CMD.close_forge); break;
    case 'close_angler': w.u8(CMD.close_angler); break;
    case 'angler_buy': w.u8(CMD.angler_buy); w.strU8(obj.item); break;
    case 'forge':      w.u8(CMD.forge); w.strU8(obj.item); break;
    case 'close_player_trade': w.u8(CMD.close_player_trade); break;
    case 'trade_offer_slot': w.u8(CMD.trade_offer_slot); w.u8(obj.slot|0); break;
    case 'trade_accept': w.u8(CMD.trade_accept); break;
    case 'trade_confirm': w.u8(CMD.trade_confirm); break;
    case 'angel_confirm': w.u8(CMD.angel_confirm); break;
    case 'angel_decline': w.u8(CMD.angel_decline); break;
    case 'chat':       w.u8(CMD.chat); w.strU16(obj.text || ''); break;
    case 'join':       return null; // join goes separately
    default: console.warn('unknown cmd', obj); return null;
  }
  return w.toBytes();
}

function encodeJoin(uuid, name) {
  const w = new BufWriter();
  w.u8(MSG_CLIENT_JOIN);
  w.strU8(uuid || '');
  w.strU8(name || 'Adventurer');
  return w.toBytes();
}

function encodeAck(tick) {
  const w = new BufWriter();
  w.u8(MSG_CLIENT_ACK);
  w.u32(tick >>> 0);
  return w.toBytes();
}

// ─── Pool mirror (client-side) ──────────────────────────────────────────────

class NetState {
  constructor() {
    this.tick = 0;
    this.tickMs = 200;
    this.w = 0;
    this.h = 0;
    this.tiles = [];     // array of tile name strings, by tile idx
    this.yourPid = 0;
    this.uuid = '';
    // Pool mirrors: sparse-by-pool-idx
    this.players = new Map();   // idx -> { id, x, y, hp, hp_max, name, skills, inv, equipment, intent, target, ui }
    this.mobs = new Map();      // idx -> { id, x, y, hp, hp_max, kind }
    this.ground = new Map();    // idx -> { id, x, y, item, qty }
    this.objects = [];          // tile-indexed array of {kind, ...}
    // From catalogs
    this.shop = []; this.sells = []; this.forge = []; this.angler = [];
    // From youview
    this.you_view = null;
    // Latest tick's events + flushed log/chat (not persistent — consumed once)
    this.events = [];
    this.log = [];
    this.chat = [];
    this.lastBaselineTick = 0;
  }

  applyInit(r) {
    /* u32 schema_hash, u16 w, u16 h, u16 your_pid, strU8 uuid, u32 tile_count, tile_count u8 tile bytes */
    const schema = r.u32(); // not strictly verified client-side; UI just runs
    this.schemaHash = schema;
    this.w = r.u16();
    this.h = r.u16();
    this.yourPid = r.u16();
    this.uuid = r.strU8();
    const n = r.u32();
    const tileBytes = r.bytes(n);
    this.tiles = new Array(n);
    for (let i = 0; i < n; i++) {
      this.tiles[i] = TILE_NAMES[tileBytes[i]] || 'grass';
    }
    this.objects = new Array(n).fill(null).map(() => ({ kind: 'none' }));
  }

  applyBaseline(r) {
    this.tick = r.u32();
    this.lastBaselineTick = this.tick;
    // wipe pools; baseline emits spawn for everything live
    this.players.clear();
    this.mobs.clear();
    this.ground.clear();
    this._readRecords(r);
  }

  applyDelta(r) {
    this.tick = r.u32();
    r.u32(); // last_acked_tick (informational)
    this._readRecords(r);
  }

  _readRecords(r) {
    while (true) {
      // peek terminator: server writes 0xFF as a full byte. terminator may also be at end of buffer
      if (r.pos >= r.buf.byteLength) break;
      const header = r.u8();
      if (header === POOL_TERMINATOR) break;
      const pool = (header >> 5) & 0x07;
      const op = (header >> 3) & 0x03;
      const largeIdx = (header & 0x01) !== 0;
      const idx = largeIdx ? r.u32() : r.u16();
      if (op === OP_DESPAWN) {
        if (pool === POOL_PLAYER) this.players.delete(idx);
        else if (pool === POOL_MOB) this.mobs.delete(idx);
        else if (pool === POOL_GROUND) this.ground.delete(idx);
        continue;
      }
      const mask = r.u16();
      if (pool === POOL_PLAYER) this._applyPlayer(r, idx, op, mask);
      else if (pool === POOL_MOB) this._applyMob(r, idx, op, mask);
      else if (pool === POOL_GROUND) this._applyGround(r, idx, op, mask);
      else if (pool === POOL_OBJECT) this._applyObject(r, idx, mask);
    }
  }

  _applyPlayer(r, idx, op, mask) {
    let p = this.players.get(idx);
    if (!p) {
      p = { id: idx, x: 0, y: 0, hp: 10, hp_max: 10, name: '', skills: null, inv: [], equipment: {}, intent: { k: 'none' }, target: null, ui: { trade_open: false, forge_open: false, angler_open: false, angel_modal_open: false } };
      this.players.set(idx, p);
    }
    if (mask & PC_X) p.x = r.i16();
    if (mask & PC_Y) p.y = r.i16();
    if (mask & PC_HP_CUR) p.hp = r.i16();
    if (mask & PC_HP_MAX) p.hp_max = r.i16();
    if (mask & PC_NAME) p.name = r.strU16();
    if (mask & PC_SKILLS) p.skills = JSON.parse(r.strU16() || 'null');
    if (mask & PC_INV) p.inv = JSON.parse(r.strU16() || '[]');
    if (mask & PC_EQUIP) p.equipment = JSON.parse(r.strU16() || '{}');
    if (mask & PC_INTENT) p.intent = JSON.parse(r.strU16() || '{"k":"none"}');
    if (mask & PC_TARGET) p.target = JSON.parse(r.strU16() || 'null');
    if (mask & PC_UI) p.ui = JSON.parse(r.strU16() || '{}');
  }

  _applyMob(r, idx, op, mask) {
    let m = this.mobs.get(idx);
    if (!m) {
      m = { id: idx, x: 0, y: 0, hp: 1, hp_max: 1, kind: 'goblin' };
      this.mobs.set(idx, m);
    }
    if (mask & MC_X) m.x = r.i16();
    if (mask & MC_Y) m.y = r.i16();
    if (mask & MC_HP_CUR) m.hp = r.i16();
    if (mask & MC_HP_MAX) m.hp_max = r.i16();
    if (mask & MC_KIND) m.kind = MOB_KINDS[r.u8()] || 'goblin';
  }

  _applyGround(r, idx, op, mask) {
    let g = this.ground.get(idx);
    if (!g) {
      g = { id: idx, x: 0, y: 0, item: '', qty: 0 };
      this.ground.set(idx, g);
    }
    if (mask & GC_X) g.x = r.i16();
    if (mask & GC_Y) g.y = r.i16();
    if (mask & GC_ITEM) g.item = r.strU16();
    if (mask & GC_QTY) g.qty = r.i32();
  }

  _applyObject(r, idx, mask) {
    const j = r.strU16();
    try {
      this.objects[idx] = JSON.parse(j) || { kind: 'none' };
    } catch { this.objects[idx] = { kind: 'none' }; }
  }

  applyEvents(r) {
    r.u32(); // tick
    const n = r.u16();
    for (let i = 0; i < n; i++) {
      const k = r.u8();
      const ev = { x: r.i16(), y: r.i16() };
      if (k === 0) ev.k = 'chop';
      else if (k === 1) ev.k = 'mine';
      else if (k === 2) ev.k = 'pick';
      else if (k === 3) ev.k = 'fish';
      else if (k === 4) { ev.k = 'hit_mob'; ev.dmg = r.i16(); }
      else if (k === 5) ev.k = 'miss_mob';
      else if (k === 6) { ev.k = 'hit_player'; ev.dmg = r.i16(); }
      else if (k === 7) ev.k = 'miss_player';
      this.events.push(ev);
    }
  }

  applyLog(r) {
    const n = r.u16();
    for (let i = 0; i < n; i++) this.log.push(r.strU16());
  }

  applyChat(r) {
    const n = r.u16();
    for (let i = 0; i < n; i++) {
      const id = r.u32();
      const tick = r.u32();
      const pid = r.u32();
      const name = r.strU16();
      const text = r.strU16();
      this.chat.push({ id, tick, pid, name, text });
    }
    // dedup by id (server resends recent chat each tick)
    const seen = new Set();
    this.chat = this.chat.filter(m => {
      if (seen.has(m.id)) return false;
      seen.add(m.id);
      return true;
    });
    // cap
    if (this.chat.length > 200) this.chat = this.chat.slice(-200);
  }

  applyCatalogs(r) {
    const blob = r.strU16();
    try {
      const o = JSON.parse(blob);
      this.shop = o.shop || [];
      this.sells = o.sells || [];
      this.forge = o.forge || [];
      this.angler = o.angler || [];
    } catch (err) { console.error('bad catalogs', err); }
  }

  applyYouView(r) {
    const blob = r.strU16();
    try {
      this.you_view = JSON.parse(blob);
    } catch { this.you_view = null; }
  }

  /** Build a legacy-shaped `state` object the existing main.js render/UI code consumes. */
  buildLegacyState() {
    const youIdx = this.yourPid;
    const youPool = this.players.get(youIdx);
    if (!youPool) return null;
    const yv = this.you_view || {};
    const players = [];
    for (const p of this.players.values()) {
      players.push({ id: p.id, x: p.x, y: p.y, name: p.name, hp: p.hp, hp_max: p.hp_max });
    }
    const mobs = [];
    for (const m of this.mobs.values()) {
      mobs.push({ id: m.id, kind: m.kind, x: m.x, y: m.y, hp: m.hp, hp_max: m.hp_max });
    }
    const ground = [];
    for (const g of this.ground.values()) {
      if (g.item) ground.push({ id: g.id, x: g.x, y: g.y, item: g.item, qty: g.qty });
    }
    return {
      tick: this.tick,
      tick_ms: this.tickMs,
      you: {
        id: youIdx,
        x: youPool.x,
        y: youPool.y,
        hp: youPool.hp,
        skills: youPool.skills || {},
        inv: youPool.inv || [],
        equipment: youPool.equipment || {},
        axe_tier: yv.axe_tier || 0,
        pickaxe_tier: yv.pickaxe_tier || 0,
        rod_tier: yv.rod_tier || 0,
        intent: youPool.intent || { k: 'none' },
        target: youPool.target,
        trade_open: !!yv.trade_open,
        angel_modal_open: !!yv.angel_modal_open,
        forge_open: !!yv.forge_open,
        angler_open: !!yv.angler_open,
        armor_defence: yv.armor_defence || 0,
        weapon_damage: yv.weapon_damage || 0,
      },
      players,
      mobs,
      objects: this.objects,
      ground,
      shop: this.shop,
      sells: this.sells,
      forge: this.forge,
      angler: this.angler,
      player_trade: yv.player_trade || null,
      chat: this.chat,
      events: this.events,
      log: this.log,
    };
  }

  // Called by the consumer after a state has been delivered, to drain transient
  // fields (events/log already shown).
  consumeTransient() {
    this.events = [];
    this.log = [];
  }
}

// ─── Public connect() — returns a WS-shaped object ──────────────────────────

export async function connectNet({ url, certHash, uuid, name }) {
  const shim = {
    readyState: 0,
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
    send(jsonStr) {
      let obj;
      try { obj = JSON.parse(jsonStr); } catch { return; }
      const bin = encodeCommand(obj);
      if (bin && writer && shim.readyState === 1) {
        writeFrame(writer, bin).catch(err => console.warn('write', err));
      }
    },
    close() {
      try { wt && wt.close(); } catch {}
      shim.readyState = 3;
      if (shim.onclose) shim.onclose();
    },
  };

  const wtOpts = certHash ? { serverCertificateHashes: [{ algorithm: 'sha-256', value: hexToBytes(certHash) }] } : undefined;
  let wt;
  try {
    wt = new WebTransport(url, wtOpts);
  } catch (err) {
    console.error('WebTransport not supported:', err);
    if (shim.onerror) shim.onerror(err);
    return shim;
  }
  await wt.ready;

  // Open a bi-di stream as the control stream
  const stream = await wt.createBidirectionalStream();
  const writer = stream.writable.getWriter();
  const reader = stream.readable.getReader();

  // Send join immediately
  await writeFrame(writer, encodeJoin(uuid, name));

  shim.readyState = 1;
  if (shim.onopen) shim.onopen();

  const net = new NetState();

  // Reader loop
  (async () => {
    let stash = new Uint8Array(0);
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      stash = concat(stash, value);
      while (true) {
        if (stash.byteLength < 4) break;
        const len = new DataView(stash.buffer, stash.byteOffset, stash.byteLength).getUint32(0, true);
        if (stash.byteLength < 4 + len) break;
        const frame = stash.slice(4, 4 + len);
        stash = stash.slice(4 + len);
        dispatchFrame(frame);
      }
    }
    shim.readyState = 3;
    if (shim.onclose) shim.onclose();
  })().catch(err => {
    console.error('read loop', err);
    shim.readyState = 3;
    if (shim.onclose) shim.onclose();
  });

  function dispatchFrame(frame) {
    const r = new BufReader(frame.buffer, frame.byteOffset);
    r.buf = frame.buffer; r.dv = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    r.pos = 0;
    // Wrap in a fresh ArrayBuffer view so positions are local
    const ab = frame.slice().buffer;
    const r2 = new BufReader(ab);
    const t = r2.u8();
    let pushed = false;
    switch (t) {
      case MSG_SERVER_INIT:
        net.applyInit(r2);
        if (shim.onmessage) shim.onmessage({ data: JSON.stringify({ t: 'init', w: net.w, h: net.h, tiles: net.tiles, you: net.yourPid, uuid: net.uuid }) });
        break;
      case MSG_SERVER_CATALOGS: net.applyCatalogs(r2); break;
      case MSG_SERVER_BASELINE: net.applyBaseline(r2); pushed = true; break;
      case MSG_SERVER_DELTA:    net.applyDelta(r2); pushed = true; break;
      case MSG_SERVER_YOUVIEW:  net.applyYouView(r2); pushed = true; break;
      case MSG_SERVER_EVENTS:   net.applyEvents(r2); pushed = true; break;
      case MSG_SERVER_LOG:      net.applyLog(r2); pushed = true; break;
      case MSG_SERVER_CHAT:     net.applyChat(r2); pushed = true; break;
      default: console.warn('unknown msg type', t);
    }
    if (pushed) {
      // Ack the latest tick
      writeFrame(writer, encodeAck(net.tick)).catch(()=>{});
      maybePushState();
    }
  }

  // Debounce state pushes: state arrives in multiple frames per tick (delta, events,
  // youview, log, chat). Coalesce into a single state event after the microtask.
  let pendingPush = false;
  function maybePushState() {
    if (pendingPush) return;
    pendingPush = true;
    queueMicrotask(() => {
      pendingPush = false;
      const state = net.buildLegacyState();
      if (!state) return;
      if (shim.onmessage) shim.onmessage({ data: JSON.stringify({ t: 'state', ...state }) });
      net.consumeTransient();
    });
  }

  return shim;
}

async function writeFrame(writer, payload) {
  const len = new Uint8Array(4);
  new DataView(len.buffer).setUint32(0, payload.byteLength, true);
  const frame = new Uint8Array(4 + payload.byteLength);
  frame.set(len, 0);
  frame.set(payload, 4);
  await writer.write(frame);
}

function concat(a, b) {
  const out = new Uint8Array(a.byteLength + b.byteLength);
  out.set(a, 0);
  out.set(b, a.byteLength);
  return out;
}

function hexToBytes(hex) {
  const clean = hex.trim().replace(/[^0-9a-fA-F]/g, '');
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export async function fetchCertHash() {
  try {
    const res = await fetch('./cert_hash.txt');
    if (!res.ok) return null;
    return (await res.text()).trim();
  } catch { return null; }
}

export function buildWtUrl() {
  // WebTransport requires HTTPS; with a self-signed cert + serverCertificateHashes,
  // Chrome accepts even from an http page. We always go direct to the server's UDP
  // port — Caddy can't reverse-proxy WebTransport (it's HTTP/3 over QUIC), only
  // the page itself is served via Caddy.
  const host = location.hostname || 'localhost';
  const port = (window.TRADSCAPE_WT_PORT) || 8082;
  return `https://${host}:${port}/wt`;
}
