use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Mutex},
};
use tower_http::services::ServeDir;
use uuid::Uuid;

const TICK_MS: u64 = 200;
const TPS: u64 = 1000 / TICK_MS;
const INV_SIZE: usize = 28;
const DB_PATH: &str = "tradscape.sqlite3";
/// Ocean columns west of the original 74-wide map; legacy map X is shifted by this offset.
const MAP_WEST_PAD: i32 = 18;

type Pid = u64;
type Mid = u64;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
fn new_pid() -> Pid {
    NEXT_PID.fetch_add(1, Ordering::Relaxed)
}

static RNG: AtomicU64 = AtomicU64::new(0xdead_beef_cafe_babe);
fn rand_u64() -> u64 {
    let mut x = RNG.load(Ordering::Relaxed);
    if x == 0 {
        x = 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG.store(x, Ordering::Relaxed);
    x
}
fn rand_f() -> f32 {
    (rand_u64() as f64 / u64::MAX as f64) as f32
}
fn rand_range(n: i32) -> i32 {
    if n <= 0 {
        0
    } else {
        (rand_u64() % n as u64) as i32
    }
}

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Tile {
    Grass,
    Dirt,
    Sand,
    Water,
    Stone,
    Path,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Obj {
    None,
    Tree { tier: i32, hp: i32 },
    Stump { tier: i32, regrow: u64 },
    Rock { tier: i32, hp: i32 },
    DepletedRock { tier: i32, regrow: u64 },
    Bush { berries: i32, regrow: u64 },
    Boulder,
    Trader,
    Angel,
}

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(tag = "k", rename_all = "snake_case")]
enum Intent {
    None,
    Chop,
    Mine,
    Pick,
    Fish,
    Talk,
    Pickup,
    Attack { mid: Mid },
    Trade { pid: Pid },
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Skills {
    woodcutting: i32,
    mining: i32,
    fishing: i32,
    attack: i32,
    strength: i32,
    defence: i32,
    hp: i32,
    woodcutting_xp: i32,
    mining_xp: i32,
    fishing_xp: i32,
    attack_xp: i32,
    strength_xp: i32,
    defence_xp: i32,
    hp_xp: i32,
    angel_points: i32,
}
impl Skills {
    fn starter() -> Self {
        Self {
            woodcutting: 1,
            mining: 1,
            fishing: 1,
            attack: 1,
            strength: 1,
            defence: 1,
            hp: 10,
            angel_points: 0,
            ..Default::default()
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct InvSlot {
    item: String,
    qty: i32,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Equipment {
    helmet: String,
    chest: String,
    legs: String,
    left_hand: String,
    right_hand: String,
}

const EQUIP_SLOT_NAMES: [&str; 5] = ["helmet", "chest", "legs", "left_hand", "right_hand"];

impl Equipment {
    fn get(&self, slot: &str) -> &str {
        match slot {
            "helmet" => &self.helmet,
            "chest" => &self.chest,
            "legs" => &self.legs,
            "left_hand" => &self.left_hand,
            "right_hand" => &self.right_hand,
            _ => "",
        }
    }
    fn set(&mut self, slot: &str, item: String) {
        match slot {
            "helmet" => self.helmet = item,
            "chest" => self.chest = item,
            "legs" => self.legs = item,
            "left_hand" => self.left_hand = item,
            "right_hand" => self.right_hand = item,
            _ => {}
        }
    }
}

/// Items in `slot` go to which equipment slot, if any.
fn item_equip_slot(item: &str) -> Option<&'static str> {
    if item.ends_with("_axe") || item.ends_with("_pickaxe") || item == "fishing_rod" {
        Some("right_hand")
    } else {
        None
    }
}

#[derive(Clone, Serialize)]
struct GroundItem {
    id: u64,
    x: i32,
    y: i32,
    item: String,
    qty: i32,
}

#[derive(Clone, Serialize)]
struct ChatMsg {
    id: u64,
    tick: u64,
    pid: Pid,
    name: String,
    text: String,
}

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TradeStage {
    Offer,
    Confirm,
}

struct Player {
    id: Pid,
    uuid: String,
    name: String,
    x: i32,
    y: i32,
    hp_cur: i32,
    skills: Skills,
    inv: Vec<InvSlot>,
    equipment: Equipment,
    target: Option<(i32, i32)>,
    intent: Intent,
    trade_open: bool,
    trade_request_from: Option<Pid>,
    trade_partner: Option<Pid>,
    trade_stage: TradeStage,
    trade_offer: Vec<usize>,
    trade_accepted: bool,
    trade_confirmed: bool,
    angel_modal_open: bool,
    private_chat: Vec<ChatMsg>,
    log: Vec<String>,
    tx: mpsc::UnboundedSender<String>,
    regen_ctr: u32,
}
impl Player {
    fn new(
        id: Pid,
        uuid: String,
        name: String,
        x: i32,
        y: i32,
        tx: mpsc::UnboundedSender<String>,
    ) -> Self {
        let inv = vec![InvSlot::default(); INV_SIZE];
        Self {
            id,
            uuid,
            name,
            x,
            y,
            hp_cur: 10,
            skills: Skills::starter(),
            inv,
            equipment: Equipment::default(),
            target: None,
            intent: Intent::None,
            trade_open: false,
            trade_request_from: None,
            trade_partner: None,
            trade_stage: TradeStage::Offer,
            trade_offer: vec![],
            trade_accepted: false,
            trade_confirmed: false,
            angel_modal_open: false,
            private_chat: vec![],
            log: vec![],
            tx,
            regen_ctr: 0,
        }
    }
}

struct Mob {
    id: Mid,
    kind: String,
    x: i32,
    y: i32,
    hp_cur: i32,
    hp_max: i32,
    attack: i32,
    strength: i32,
    defence: i32,
    home: (i32, i32),
    respawn_at: Option<u64>,
}

struct MobSpawn {
    kind: &'static str,
    x: i32,
    y: i32,
}

#[derive(Clone, Copy)]
struct MobDef {
    kind: &'static str,
    name: &'static str,
    tier: i32,
    hp: i32,
    attack: i32,
    strength: i32,
    defence: i32,
    aggro: i32,
    coin: i32,
}

const MOB_DEFS: [MobDef; 4] = [
    MobDef { kind: "goblin",      name: "goblin",      tier: 1, hp: 24,  attack: 8,  strength: 9,   defence: 4,  aggro: 6, coin: 5   },
    MobDef { kind: "club_goblin", name: "club goblin", tier: 2, hp: 60,  attack: 18, strength: 22,  defence: 12, aggro: 6, coin: 25  },
    MobDef { kind: "ninja",       name: "ninja",       tier: 3, hp: 140, attack: 45, strength: 50,  defence: 35, aggro: 8, coin: 90  },
    MobDef { kind: "dragon",      name: "dragon",      tier: 4, hp: 350, attack: 90, strength: 100, defence: 80, aggro: 6, coin: 225 },
];

fn mob_def(kind: &str) -> MobDef {
    MOB_DEFS.iter().find(|m| m.kind == kind).copied().unwrap_or(MOB_DEFS[0])
}

/// Drop tools of mob's tier; each tool has chance `0.05 / 4^tier`.
fn roll_mob_drops(mob_tier: i32) -> Vec<(&'static str, i32)> {
    let chance = 0.05_f32 / 4.0_f32.powi(mob_tier);
    let mut drops = Vec::new();
    for t in TOOL_DEFS.iter().filter(|t| t.tier == mob_tier) {
        if rand_f() < chance {
            drops.push((t.item, 1));
        }
    }
    drops
}

struct MapDef {
    w: i32,
    h: i32,
    tiles: Vec<Tile>,
    objects: Vec<Obj>,
    mobs: Vec<MobSpawn>,
    player_spawn: (i32, i32),
}

include!(concat!(env!("OUT_DIR"), "/generated_map.rs"));

struct Game {
    w: i32,
    h: i32,
    tiles: Vec<Tile>,
    objects: Vec<Obj>,
    player_spawn: (i32, i32),
    players: HashMap<Pid, Player>,
    mobs: HashMap<Mid, Mob>,
    db: Connection,
    tick: u64,
    next_mid: Mid,
    chat_seq: u64,
    chat: VecDeque<ChatMsg>,
    events: Vec<Value>,
    ground: Vec<GroundItem>,
    next_gid: u64,
}

impl Game {
    fn new() -> Self {
        let MapDef {
            w,
            h,
            tiles,
            objects,
            mobs,
            player_spawn,
        } = build_map();
        let db = open_db();
        let mut g = Self {
            w,
            h,
            tiles,
            objects,
            player_spawn,
            players: HashMap::new(),
            mobs: HashMap::new(),
            db,
            tick: 0,
            next_mid: 1,
            chat_seq: 0,
            chat: VecDeque::new(),
            events: Vec::new(),
            ground: Vec::new(),
            next_gid: 1,
        };
        for mob in mobs {
            g.spawn_mob(mob.kind, mob.x, mob.y);
        }
        g
    }
    fn spawn_mob(&mut self, kind: &str, x: i32, y: i32) {
        let id = self.next_mid;
        self.next_mid += 1;
        let d = mob_def(kind);
        self.mobs.insert(
            id,
            Mob {
                id,
                kind: kind.into(),
                x,
                y,
                hp_cur: d.hp,
                hp_max: d.hp,
                attack: d.attack,
                strength: d.strength,
                defence: d.defence,
                home: (x, y),
                respawn_at: None,
            },
        );
    }
    fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.w + x) as usize
    }
    fn in_b(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h
    }
    fn tile(&self, x: i32, y: i32) -> Tile {
        self.tiles[self.idx(x, y)]
    }
    fn obj(&self, x: i32, y: i32) -> &Obj {
        &self.objects[self.idx(x, y)]
    }
    fn set_obj(&mut self, x: i32, y: i32, o: Obj) {
        let i = self.idx(x, y);
        self.objects[i] = o;
    }
    fn drop_ground(&mut self, x: i32, y: i32, item: &str, qty: i32) {
        if qty <= 0 || item.is_empty() {
            return;
        }
        if let Some(gi) = self.ground.iter_mut().find(|g| g.x == x && g.y == y && g.item == item) {
            gi.qty += qty;
            return;
        }
        let id = self.next_gid;
        self.next_gid += 1;
        self.ground.push(GroundItem { id, x, y, item: item.into(), qty });
    }
    fn occupant_pid(&self, x: i32, y: i32) -> Option<Pid> {
        self.players
            .values()
            .find(|p| p.x == x && p.y == y)
            .map(|p| p.id)
    }
    fn occupant_mid(&self, x: i32, y: i32) -> Option<Mid> {
        self.mobs
            .values()
            .find(|m| m.respawn_at.is_none() && m.x == x && m.y == y)
            .map(|m| m.id)
    }
    fn walkable(&self, x: i32, y: i32, ignore_pid: Pid) -> bool {
        if !self.in_b(x, y) {
            return false;
        }
        if matches!(self.tile(x, y), Tile::Water) {
            return false;
        }
        if !matches!(self.obj(x, y), Obj::None) {
            return false;
        }
        if let Some(pid) = self.occupant_pid(x, y) {
            if pid != ignore_pid {
                return false;
            }
        }
        if self.occupant_mid(x, y).is_some() {
            return false;
        }
        true
    }
}

struct SavedPlayer {
    name: String,
    x: i32,
    y: i32,
    hp_cur: i32,
    skills: Skills,
    inv: Vec<InvSlot>,
    equipment: Equipment,
}

fn open_db() -> Connection {
    let db = Connection::open(DB_PATH).expect("open sqlite database");
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS players (
            uuid TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            x INTEGER NOT NULL,
            y INTEGER NOT NULL,
            hp_cur INTEGER NOT NULL,
            skills_json TEXT NOT NULL,
            inv_json TEXT NOT NULL,
            equipment_json TEXT NOT NULL DEFAULT '{}',
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );",
    )
    .expect("create players table");
    let ver: i32 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if ver < 1 {
        // Shift saved positions east once — map gained MAP_WEST_PAD columns west (legacy width was 74).
        let _ = db.execute(
            "UPDATE players SET x = x + ?1 WHERE x <= ?2",
            params![MAP_WEST_PAD, 73_i32],
        );
        let _ = db.execute_batch("PRAGMA user_version = 1");
    }
    if ver < 2 {
        let _ = db.execute_batch(
            "ALTER TABLE players ADD COLUMN equipment_json TEXT NOT NULL DEFAULT '{}';
             PRAGMA user_version = 2;",
        );
    }
    db
}

fn clean_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(20)
        .collect::<String>()
        .trim()
        .to_string();
    if clean.is_empty() {
        "Adventurer".into()
    } else {
        clean
    }
}

fn valid_uuid_or_new(raw: Option<&str>) -> String {
    raw.and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::new_v4)
        .to_string()
}

fn load_player(db: &Connection, uuid: &str) -> Option<SavedPlayer> {
    db.query_row(
        "SELECT name, x, y, hp_cur, skills_json, inv_json, equipment_json FROM players WHERE uuid = ?1",
        params![uuid],
        |row| {
            let skills_json: String = row.get(4)?;
            let inv_json: String = row.get(5)?;
            let equipment_json: String = row.get(6)?;
            let equipment: Equipment = serde_json::from_str(&equipment_json).unwrap_or_default();
            let mut skills =
                serde_json::from_str(&skills_json).unwrap_or_else(|_| Skills::starter());
            skills.woodcutting = level_from_xp(skills.woodcutting_xp);
            skills.mining = level_from_xp(skills.mining_xp);
            skills.fishing = level_from_xp(skills.fishing_xp);
            skills.attack = level_from_xp(skills.attack_xp);
            skills.strength = level_from_xp(skills.strength_xp);
            skills.defence = level_from_xp(skills.defence_xp);
            skills.hp = 10 + level_from_xp(skills.hp_xp) - 1;
            let mut inv: Vec<InvSlot> = serde_json::from_str(&inv_json)
                .unwrap_or_else(|_| vec![InvSlot::default(); INV_SIZE]);
            inv.resize(INV_SIZE, InvSlot::default());
            Ok(SavedPlayer {
                name: row.get(0)?,
                x: row.get(1)?,
                y: row.get(2)?,
                hp_cur: row.get(3)?,
                skills,
                inv,
                equipment,
            })
        },
    )
    .optional()
    .unwrap_or(None)
}

#[derive(Clone)]
struct PlayerSave {
    uuid: String,
    name: String,
    x: i32,
    y: i32,
    hp_cur: i32,
    skills: Skills,
    inv: Vec<InvSlot>,
    equipment: Equipment,
}

impl From<&Player> for PlayerSave {
    fn from(p: &Player) -> Self {
        Self {
            uuid: p.uuid.clone(),
            name: p.name.clone(),
            x: p.x,
            y: p.y,
            hp_cur: p.hp_cur,
            skills: p.skills.clone(),
            inv: p.inv.clone(),
            equipment: p.equipment.clone(),
        }
    }
}

fn save_player(db: &Connection, p: &Player) {
    let rec = PlayerSave::from(p);
    save_player_record(db, &rec);
}

fn save_player_record(db: &Connection, rec: &PlayerSave) {
    let skills_json = serde_json::to_string(&rec.skills).unwrap_or_else(|_| "{}".into());
    let inv_json = serde_json::to_string(&rec.inv).unwrap_or_else(|_| "[]".into());
    let equipment_json = serde_json::to_string(&rec.equipment).unwrap_or_else(|_| "{}".into());
    let _ = db.execute(
        "INSERT INTO players (uuid, name, x, y, hp_cur, skills_json, inv_json, equipment_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, unixepoch())
         ON CONFLICT(uuid) DO UPDATE SET
            name = excluded.name,
            x = excluded.x,
            y = excluded.y,
            hp_cur = excluded.hp_cur,
            skills_json = excluded.skills_json,
            inv_json = excluded.inv_json,
            equipment_json = excluded.equipment_json,
            updated_at = excluded.updated_at",
        params![
            &rec.uuid,
            &rec.name,
            rec.x,
            rec.y,
            rec.hp_cur,
            &skills_json,
            &inv_json,
            &equipment_json
        ],
    );
}

fn save_player_without_game_lock(rec: &PlayerSave) {
    if let Ok(db) = Connection::open(DB_PATH) {
        save_player_record(&db, rec);
    }
}

fn manhattan(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}
fn chebyshev(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs())
}

#[derive(Clone, Copy)]
enum GoalKind {
    Step((i32, i32)),
    Adjacent((i32, i32)),
}

fn bfs(
    g: &Game,
    from: (i32, i32),
    goal: GoalKind,
    ignore_pid: Pid,
) -> Option<VecDeque<(i32, i32)>> {
    let blocked = match goal {
        GoalKind::Adjacent(t) => Some(t),
        _ => None,
    };
    let goal_test = |p: (i32, i32)| match goal {
        GoalKind::Step(t) => p == t,
        GoalKind::Adjacent(t) => chebyshev(p, t) == 1,
    };
    if goal_test(from) {
        return Some(VecDeque::new());
    }
    let mut prev: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    let mut q = VecDeque::new();
    q.push_back(from);
    prev.insert(from, from);
    while let Some(cur) = q.pop_front() {
        for (dx, dy) in [
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let n = (cur.0 + dx, cur.1 + dy);
            if prev.contains_key(&n) {
                continue;
            }
            if Some(n) == blocked {
                continue;
            }
            if !g.walkable(n.0, n.1, ignore_pid) {
                continue;
            }
            // anti-corner-cut: diagonal moves require both orthogonal neighbors walkable
            if dx != 0 && dy != 0 {
                if !g.walkable(cur.0 + dx, cur.1, ignore_pid) {
                    continue;
                }
                if !g.walkable(cur.0, cur.1 + dy, ignore_pid) {
                    continue;
                }
            }
            prev.insert(n, cur);
            if goal_test(n) {
                let mut path = VecDeque::new();
                let mut c = n;
                while c != from {
                    path.push_front(c);
                    c = prev[&c];
                }
                return Some(path);
            }
            q.push_back(n);
        }
    }
    None
}

/// Cumulative XP needed to **reach** this combat/gathering level (level 1 ⇔ 0 XP).
/// Each step `(L → L+1)` costs `BASE * MULT^(L-1)` — requirement grows exponentially; total XP is unbounded.
const XP_CURVE_BASE: f64 = 50.0;
const XP_CURVE_MULT: f64 = 1.17;
const XP_MAX_LEVEL: i32 = 99;

fn xp_threshold_for_level(level: i32) -> i64 {
    if level <= 1 {
        return 0;
    }
    let steps = (level - 1) as f64;
    let numer = XP_CURVE_MULT.powf(steps) - 1.0;
    let denom = XP_CURVE_MULT - 1.0;
    (XP_CURVE_BASE * numer / denom).floor() as i64
}

/// Skill level from lifetime XP (same curve for woodcutting, mining, fishing, melee stats).
fn level_from_xp(xp: i32) -> i32 {
    let xp = xp.max(0) as i64;
    let mut lo = 1i32;
    let mut hi = XP_MAX_LEVEL;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if xp_threshold_for_level(mid) <= xp {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

fn log_level_up(log: &mut Vec<String>, skill: &str, old_level: i32, new_level: i32) {
    if new_level > old_level {
        log.push(format!("Level up! {} is now {}.", skill, new_level));
    }
}

#[derive(Clone, Copy)]
struct ResourceDef {
    tier: i32,
    name: &'static str,
    item: &'static str,
    hp: i32,
    xp: i32,
    sell: i32,
    req_tool_tier: i32,
    regrow_secs: u64,
}

#[derive(Clone, Copy)]
struct ToolDef {
    item: &'static str,
    name: &'static str,
    kind: &'static str,
    tier: i32,
    buy: i32,
    power: i32,
}

const TREE_DEFS: [ResourceDef; 4] = [
    ResourceDef {
        tier: 1,
        name: "Pine Tree",
        item: "pine_logs",
        hp: 4,
        xp: 20,
        sell: 5,
        req_tool_tier: 1,
        regrow_secs: 25,
    },
    ResourceDef {
        tier: 2,
        name: "Oak Tree",
        item: "oak_logs",
        hp: 10,
        xp: 55,
        sell: 50,
        req_tool_tier: 2,
        regrow_secs: 45,
    },
    ResourceDef {
        tier: 3,
        name: "Yew Tree",
        item: "yew_logs",
        hp: 22,
        xp: 130,
        sell: 500,
        req_tool_tier: 3,
        regrow_secs: 75,
    },
    ResourceDef {
        tier: 4,
        name: "Magic Tree",
        item: "magic_logs",
        hp: 48,
        xp: 320,
        sell: 5000,
        req_tool_tier: 4,
        regrow_secs: 110,
    },
];

const ROCK_DEFS: [ResourceDef; 4] = [
    ResourceDef {
        tier: 1,
        name: "Copper Rock",
        item: "copper_ore",
        hp: 5,
        xp: 25,
        sell: 6,
        req_tool_tier: 1,
        regrow_secs: 30,
    },
    ResourceDef {
        tier: 2,
        name: "Iron Rock",
        item: "iron_ore",
        hp: 12,
        xp: 65,
        sell: 60,
        req_tool_tier: 2,
        regrow_secs: 50,
    },
    ResourceDef {
        tier: 3,
        name: "Gold Rock",
        item: "gold_ore",
        hp: 26,
        xp: 150,
        sell: 600,
        req_tool_tier: 3,
        regrow_secs: 80,
    },
    ResourceDef {
        tier: 4,
        name: "Cobalt Rock",
        item: "cobalt_ore",
        hp: 54,
        xp: 340,
        sell: 6000,
        req_tool_tier: 4,
        regrow_secs: 110,
    },
];

const TOOL_DEFS: [ToolDef; 9] = [
    ToolDef {
        item: "bronze_axe",
        name: "Bronze Axe",
        kind: "axe",
        tier: 1,
        buy: 10,
        power: 1,
    },
    ToolDef {
        item: "iron_axe",
        name: "Iron Axe",
        kind: "axe",
        tier: 2,
        buy: 200,
        power: 2,
    },
    ToolDef {
        item: "steel_axe",
        name: "Steel Axe",
        kind: "axe",
        tier: 3,
        buy: 4000,
        power: 4,
    },
    ToolDef {
        item: "cobalt_axe",
        name: "Cobalt Axe",
        kind: "axe",
        tier: 4,
        buy: 30000,
        power: 8,
    },
    ToolDef {
        item: "bronze_pickaxe",
        name: "Bronze Pickaxe",
        kind: "pickaxe",
        tier: 1,
        buy: 10,
        power: 1,
    },
    ToolDef {
        item: "iron_pickaxe",
        name: "Iron Pickaxe",
        kind: "pickaxe",
        tier: 2,
        buy: 200,
        power: 2,
    },
    ToolDef {
        item: "steel_pickaxe",
        name: "Steel Pickaxe",
        kind: "pickaxe",
        tier: 3,
        buy: 4000,
        power: 4,
    },
    ToolDef {
        item: "cobalt_pickaxe",
        name: "Cobalt Pickaxe",
        kind: "pickaxe",
        tier: 4,
        buy: 30000,
        power: 8,
    },
    ToolDef {
        item: "fishing_rod",
        name: "Fishing Rod",
        kind: "rod",
        tier: 2,
        buy: 200,
        power: 0,
    },
];

fn tree_def(tier: i32) -> ResourceDef {
    let i = (tier - 1).clamp(0, TREE_DEFS.len() as i32 - 1) as usize;
    TREE_DEFS[i]
}
fn rock_def(tier: i32) -> ResourceDef {
    let i = (tier - 1).clamp(0, ROCK_DEFS.len() as i32 - 1) as usize;
    ROCK_DEFS[i]
}

fn item_name(item: &str) -> &'static str {
    match item {
        "coins" => return "Coins",
        "berries" => return "Berries",
        "pine_logs" => return "Pine logs",
        "oak_logs" => return "Oak logs",
        "yew_logs" => return "Yew logs",
        "magic_logs" => return "Magic logs",
        "copper_ore" => return "Copper ore",
        "iron_ore" => return "Iron ore",
        "gold_ore" => return "Gold ore",
        "cobalt_ore" => return "Cobalt ore",
        "salmon" => return "Salmon",
        _ => {}
    }
    if let Some(t) = TOOL_DEFS.iter().find(|t| t.item == item) {
        return t.name;
    }
    "Item"
}

fn sell_value(item: &str) -> Option<i32> {
    if item == "berries" {
        return Some(1);
    }
    if item == "salmon" {
        return Some(35);
    }
    TREE_DEFS
        .iter()
        .chain(ROCK_DEFS.iter())
        .find(|r| r.item == item)
        .map(|r| r.sell)
}

fn buy_price(item: &str) -> Option<i32> {
    TOOL_DEFS.iter().find(|t| t.item == item).map(|t| t.buy)
}

fn item_value(item: &str) -> Option<i32> {
    buy_price(item).or_else(|| sell_value(item))
}

fn xp_with_bonus(base: i32, angel_points: i32) -> i32 {
    ((base as f64) * (1.0 + angel_points as f64 * 0.01)).round() as i32
}

fn inventory_gp_value(p: &Player) -> i64 {
    let mut sum = 0i64;
    for s in &p.inv {
        if s.qty <= 0 || s.item.is_empty() {
            continue;
        }
        let unit = if s.item == "coins" {
            1i64
        } else {
            item_value(&s.item).unwrap_or(0) as i64
        };
        sum += unit * s.qty as i64;
    }
    sum
}

fn tool_def(item: &str) -> Option<ToolDef> {
    TOOL_DEFS.iter().find(|t| t.item == item).copied()
}

fn best_tool(p: &Player, kind: &str) -> Option<ToolDef> {
    p.inv
        .iter()
        .filter(|s| s.qty > 0)
        .filter_map(|s| tool_def(&s.item))
        .filter(|t| t.kind == kind)
        .max_by_key(|t| t.tier)
}

fn has_item(p: &Player, item: &str) -> bool {
    p.inv.iter().any(|s| s.item == item && s.qty > 0)
}

fn gather_success(skill: i32, resource_tier: i32) -> bool {
    let target = 1 + (resource_tier - 1) * 8;
    let chance = (0.45 + (skill - target) as f32 * 0.035).clamp(0.18, 0.92);
    rand_f() < chance
}

/// Per-tick chance to catch: **1% × Fishing level** (level 1 ⇒ 1%, level 40 ⇒ 40%).
fn fish_bite_chance(fishing_level: i32) -> f32 {
    let lvl = fishing_level.max(1).min(99);
    lvl as f32 * 0.01
}

fn add_inv(p: &mut Player, item: &str, qty: i32) -> bool {
    for s in p.inv.iter_mut() {
        if s.item == item && s.qty > 0 {
            s.qty += qty;
            return true;
        }
    }
    for s in p.inv.iter_mut() {
        if s.item.is_empty() {
            s.item = item.into();
            s.qty = qty;
            return true;
        }
    }
    false
}
fn coin_count(p: &Player) -> i32 {
    p.inv
        .iter()
        .filter(|s| s.item == "coins")
        .map(|s| s.qty)
        .sum()
}
fn deduct_coins(p: &mut Player, amt: i32) -> bool {
    if coin_count(p) < amt {
        return false;
    }
    let mut left = amt;
    for s in p.inv.iter_mut() {
        if s.item == "coins" {
            let take = left.min(s.qty);
            s.qty -= take;
            left -= take;
            if s.qty == 0 {
                *s = InvSlot::default();
            }
            if left == 0 {
                return true;
            }
        }
    }
    true
}

fn click(g: &mut Game, pid: Pid, x: i32, y: i32) {
    if !g.in_b(x, y) {
        return;
    }
    if let Some(mid) = g.occupant_mid(x, y) {
        attack(g, pid, mid);
        return;
    }
    if let Some(other_pid) = g.occupant_pid(x, y) {
        if other_pid != pid {
            trade(g, pid, other_pid);
            return;
        }
    }
    let clicked_obj = g.obj(x, y).clone();
    let mut intent = match &clicked_obj {
        Obj::Tree { .. } => Intent::Chop,
        Obj::Rock { .. } => Intent::Mine,
        Obj::Bush { berries, .. } if *berries > 0 => Intent::Pick,
        Obj::Trader => Intent::Talk,
        Obj::Angel => Intent::Talk,
        _ => Intent::None,
    };
    if matches!(intent, Intent::None) && matches!(g.tile(x, y), Tile::Water) {
        let rod_ok = g
            .players
            .get(&pid)
            .map(|p| has_item(p, "fishing_rod"))
            .unwrap_or(false);
        if rod_ok {
            intent = Intent::Fish;
        }
    }
    if matches!(intent, Intent::None) && g.ground.iter().any(|gi| gi.x == x && gi.y == y) {
        intent = Intent::Pickup;
    }
    let walk_ok = g.walkable(x, y, pid);
    cancel_player_trade(g, pid, "Trade cancelled.");
    if !matches!(intent, Intent::Talk) {
        let p = g.players.get_mut(&pid).unwrap();
        p.trade_open = false;
        p.angel_modal_open = false;
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        match clicked_obj {
            Obj::Trader => {
                p.angel_modal_open = false;
            }
            Obj::Angel => {
                p.trade_open = false;
            }
            _ => {}
        }
    }
    let p = g.players.get_mut(&pid).unwrap();
    if matches!(intent, Intent::None) {
        if walk_ok {
            p.target = Some((x, y));
            p.intent = Intent::None;
        } else {
            p.target = None;
            p.intent = Intent::None;
        }
    } else {
        p.target = Some((x, y));
        p.intent = intent;
    }
}

fn trade(g: &mut Game, pid: Pid, other_pid: Pid) {
    if pid == other_pid {
        return;
    }
    let Some(other) = g.players.get(&other_pid) else {
        return;
    };
    let (tx, ty) = (other.x, other.y);
    cancel_player_trade(g, pid, "Trade cancelled.");
    let p = g.players.get_mut(&pid).unwrap();
    p.trade_open = false;
    p.angel_modal_open = false;
    p.intent = Intent::Trade { pid: other_pid };
    p.target = Some((tx, ty));
}

fn attack(g: &mut Game, pid: Pid, mid: Mid) {
    let Some(m) = g.mobs.get(&mid) else {
        return;
    };
    if m.respawn_at.is_some() {
        return;
    }
    let target = (m.x, m.y);
    cancel_player_trade(g, pid, "Trade cancelled.");
    let p = g.players.get_mut(&pid).unwrap();
    p.trade_open = false;
    p.angel_modal_open = false;
    p.intent = Intent::Attack { mid };
    p.target = Some(target);
}

fn near_trader(g: &Game, pid: Pid) -> bool {
    let p = &g.players[&pid];
    for dy in -1..=1i32 {
        for dx in -1..=1i32 {
            let nx = p.x + dx;
            let ny = p.y + dy;
            if g.in_b(nx, ny) && matches!(g.obj(nx, ny), Obj::Trader) {
                return true;
            }
        }
    }
    false
}

fn near_angel(g: &Game, pid: Pid) -> bool {
    let p = &g.players[&pid];
    for dy in -1..=1i32 {
        for dx in -1..=1i32 {
            let nx = p.x + dx;
            let ny = p.y + dy;
            if g.in_b(nx, ny) && matches!(g.obj(nx, ny), Obj::Angel) {
                return true;
            }
        }
    }
    false
}

fn reset_player_trade_fields(p: &mut Player) {
    p.trade_partner = None;
    p.trade_stage = TradeStage::Offer;
    p.trade_offer.clear();
    p.trade_accepted = false;
    p.trade_confirmed = false;
}

fn cancel_player_trade(g: &mut Game, pid: Pid, other_msg: &str) {
    let partner = g.players.get(&pid).and_then(|p| p.trade_partner);
    if let Some(p) = g.players.get_mut(&pid) {
        reset_player_trade_fields(p);
    }
    if let Some(other_pid) = partner {
        if let Some(other) = g.players.get_mut(&other_pid) {
            if other.trade_partner == Some(pid) {
                reset_player_trade_fields(other);
                other.log.push(other_msg.into());
            }
        }
    }
}

fn begin_player_trade(g: &mut Game, a: Pid, b: Pid) {
    if !g.players.contains_key(&a) || !g.players.contains_key(&b) {
        return;
    }
    cancel_player_trade(g, a, "Trade cancelled.");
    cancel_player_trade(g, b, "Trade cancelled.");
    let a_name = g.players[&a].name.clone();
    let b_name = g.players[&b].name.clone();
    if let Some(pa) = g.players.get_mut(&a) {
        reset_player_trade_fields(pa);
        pa.trade_partner = Some(b);
        pa.trade_request_from = None;
        pa.trade_open = false;
        pa.angel_modal_open = false;
        pa.intent = Intent::None;
        pa.target = None;
        pa.log.push(format!("Trading with {}.", b_name));
    }
    if let Some(pb) = g.players.get_mut(&b) {
        reset_player_trade_fields(pb);
        pb.trade_partner = Some(a);
        pb.trade_request_from = None;
        pb.trade_open = false;
        pb.angel_modal_open = false;
        pb.intent = Intent::None;
        pb.target = None;
        pb.log.push(format!("Trading with {}.", a_name));
    }
}

fn request_player_trade(g: &mut Game, pid: Pid, other_pid: Pid) {
    if pid == other_pid || !g.players.contains_key(&pid) || !g.players.contains_key(&other_pid) {
        return;
    }
    if g.players.get(&pid).and_then(|p| p.trade_request_from) == Some(other_pid) {
        begin_player_trade(g, pid, other_pid);
        return;
    }
    let name = g.players[&pid].name.clone();
    let other_name = g.players[&other_pid].name.clone();
    if let Some(other) = g.players.get_mut(&other_pid) {
        other.trade_request_from = Some(pid);
    }
    if let Some(p) = g.players.get_mut(&pid) {
        p.log
            .push(format!("Sending trade offer to {}...", other_name));
    }
    push_private_chat(g, other_pid, format!("{} wishes to trade with you.", name));
}

fn reset_trade_accepts(g: &mut Game, pid: Pid) {
    let partner = g.players.get(&pid).and_then(|p| p.trade_partner);
    for id in [Some(pid), partner].into_iter().flatten() {
        if let Some(p) = g.players.get_mut(&id) {
            p.trade_stage = TradeStage::Offer;
            p.trade_accepted = false;
            p.trade_confirmed = false;
        }
    }
}

fn trade_offer_slot(g: &mut Game, pid: Pid, slot: usize) {
    if slot >= INV_SIZE {
        return;
    }
    let partner = g.players.get(&pid).and_then(|p| p.trade_partner);
    if partner.is_none() {
        return;
    }
    let Some(p) = g.players.get_mut(&pid) else {
        return;
    };
    if p.inv.get(slot).map(|s| s.item.is_empty()).unwrap_or(true) {
        return;
    }
    if p.trade_offer.contains(&slot) {
        p.trade_offer.retain(|s| *s != slot);
    } else {
        p.trade_offer.push(slot);
    }
    reset_trade_accepts(g, pid);
}

fn trade_accept(g: &mut Game, pid: Pid) {
    let Some(partner) = g.players.get(&pid).and_then(|p| p.trade_partner) else {
        return;
    };
    if g.players.get(&partner).and_then(|p| p.trade_partner) != Some(pid) {
        cancel_player_trade(g, pid, "Trade cancelled.");
        return;
    }
    if let Some(p) = g.players.get_mut(&pid) {
        p.trade_accepted = true;
    }
    let both = g
        .players
        .get(&pid)
        .map(|p| p.trade_accepted)
        .unwrap_or(false)
        && g.players
            .get(&partner)
            .map(|p| p.trade_accepted)
            .unwrap_or(false);
    if both {
        for id in [pid, partner] {
            if let Some(p) = g.players.get_mut(&id) {
                p.trade_stage = TradeStage::Confirm;
                p.trade_confirmed = false;
            }
        }
    }
}

fn trade_confirm(g: &mut Game, pid: Pid) {
    let Some(partner) = g.players.get(&pid).and_then(|p| p.trade_partner) else {
        return;
    };
    if g.players.get(&pid).map(|p| p.trade_stage) != Some(TradeStage::Confirm)
        || g.players.get(&partner).map(|p| p.trade_stage) != Some(TradeStage::Confirm)
    {
        return;
    }
    if let Some(p) = g.players.get_mut(&pid) {
        p.trade_confirmed = true;
    }
    let both = g
        .players
        .get(&pid)
        .map(|p| p.trade_confirmed)
        .unwrap_or(false)
        && g.players
            .get(&partner)
            .map(|p| p.trade_confirmed)
            .unwrap_or(false);
    if both {
        complete_player_trade(g, pid, partner);
    }
}

fn offer_items_from_slots(inv: &[InvSlot], slots: &[usize]) -> Option<Vec<(usize, InvSlot)>> {
    let mut out = Vec::new();
    for &slot in slots {
        if slot >= INV_SIZE || out.iter().any(|(s, _)| *s == slot) {
            return None;
        }
        let it = inv.get(slot)?.clone();
        if it.item.is_empty() || it.qty <= 0 {
            return None;
        }
        out.push((slot, it));
    }
    Some(out)
}

fn add_inv_to_slots(inv: &mut Vec<InvSlot>, item: &str, qty: i32) -> bool {
    for s in inv.iter_mut() {
        if s.item == item && s.qty > 0 {
            s.qty += qty;
            return true;
        }
    }
    for s in inv.iter_mut() {
        if s.item.is_empty() {
            s.item = item.into();
            s.qty = qty;
            return true;
        }
    }
    false
}

fn can_receive_trade_items(p: &Player, incoming: &[InvSlot], outgoing_slots: &[usize]) -> bool {
    let Some(outgoing) = offer_items_from_slots(&p.inv, outgoing_slots) else {
        return false;
    };
    let mut inv = p.inv.clone();
    for (slot, _) in outgoing {
        inv[slot] = InvSlot::default();
    }
    for it in incoming {
        if !add_inv_to_slots(&mut inv, &it.item, it.qty) {
            return false;
        }
    }
    true
}

fn complete_player_trade(g: &mut Game, a: Pid, b: Pid) {
    if g.players.get(&a).and_then(|p| p.trade_partner) != Some(b)
        || g.players.get(&b).and_then(|p| p.trade_partner) != Some(a)
    {
        return;
    }
    let a_slots = g.players[&a].trade_offer.clone();
    let b_slots = g.players[&b].trade_offer.clone();
    let Some(a_offer) = offer_items_from_slots(&g.players[&a].inv, &a_slots) else {
        cancel_player_trade(g, a, "Trade cancelled.");
        return;
    };
    let Some(b_offer) = offer_items_from_slots(&g.players[&b].inv, &b_slots) else {
        cancel_player_trade(g, b, "Trade cancelled.");
        return;
    };
    let a_items: Vec<InvSlot> = a_offer.iter().map(|(_, it)| it.clone()).collect();
    let b_items: Vec<InvSlot> = b_offer.iter().map(|(_, it)| it.clone()).collect();
    if !can_receive_trade_items(&g.players[&a], &b_items, &a_slots)
        || !can_receive_trade_items(&g.players[&b], &a_items, &b_slots)
    {
        cancel_player_trade(g, a, "Trade cancelled.");
        if let Some(pa) = g.players.get_mut(&a) {
            pa.log
                .push("Trade cancelled: not enough inventory space.".into());
        }
        return;
    }
    if let Some(pa) = g.players.get_mut(&a) {
        for (slot, _) in &a_offer {
            pa.inv[*slot] = InvSlot::default();
        }
        for it in &b_items {
            add_inv(pa, &it.item, it.qty);
        }
        reset_player_trade_fields(pa);
        pa.log.push("Accepted trade.".into());
    }
    if let Some(pb) = g.players.get_mut(&b) {
        for (slot, _) in &b_offer {
            pb.inv[*slot] = InvSlot::default();
        }
        for it in &a_items {
            add_inv(pb, &it.item, it.qty);
        }
        reset_player_trade_fields(pb);
        pb.log.push("Accepted trade.".into());
    }
}

fn angel_decline(g: &mut Game, pid: Pid) {
    if let Some(p) = g.players.get_mut(&pid) {
        p.angel_modal_open = false;
    }
}

fn angel_sacrifice(g: &mut Game, pid: Pid) {
    if !near_angel(g, pid) {
        if let Some(p) = g.players.get_mut(&pid) {
            p.log.push("Stand next to the angel.".into());
        }
        return;
    }
    let p = g.players.get_mut(&pid).unwrap();
    let gp = inventory_gp_value(p);
    let gain = (gp / 1000) as i32;
    let new_points = p.skills.angel_points.saturating_add(gain);
    p.inv = vec![InvSlot::default(); INV_SIZE];
    p.skills = Skills::starter();
    p.skills.angel_points = new_points;
    p.hp_cur = p.skills.hp;
    p.intent = Intent::None;
    p.target = None;
    p.trade_open = false;
    p.angel_modal_open = false;
    p.log.push(format!(
        "Your inventory is sacrificed ({} GP). You gain {} angel points. All levels reset. Angel points grant +1% XP each.",
        gp, gain
    ));
}

fn buy(g: &mut Game, pid: Pid, item: &str) {
    if !near_trader(g, pid) {
        if let Some(p) = g.players.get_mut(&pid) {
            p.log.push("Stand next to the trader.".into());
        }
        return;
    }
    let Some(price) = buy_price(item) else {
        return;
    };
    let p = g.players.get_mut(&pid).unwrap();
    if has_item(p, item) {
        p.log
            .push(format!("You already have a {}.", item_name(item)));
        return;
    }
    if !deduct_coins(p, price) {
        p.log.push("Not enough coins.".into());
        return;
    }
    if add_inv(p, item, 1) {
        p.log.push(format!("You buy a {}.", item_name(item)));
    } else {
        add_inv(p, "coins", price);
        p.log.push("Inventory full!".into());
    }
}
fn sell(g: &mut Game, pid: Pid, slot: usize) {
    if !near_trader(g, pid) {
        if let Some(p) = g.players.get_mut(&pid) {
            p.log.push("Stand next to the trader.".into());
        }
        return;
    }
    let p = g.players.get_mut(&pid).unwrap();
    if slot >= INV_SIZE {
        return;
    }
    let item = p.inv[slot].item.clone();
    if item.is_empty() || item == "coins" {
        return;
    }
    let Some(unit) = item_value(&item) else {
        p.log.push("The trader does not buy that.".into());
        return;
    };
    let qty = p.inv[slot].qty;
    p.inv[slot] = InvSlot::default();
    add_inv(p, "coins", unit * qty);
    p.log
        .push(format!("Sold {}x{} for {}gp.", item, qty, unit * qty));
}

fn push_private_chat(g: &mut Game, pid: Pid, text: impl Into<String>) {
    g.chat_seq += 1;
    let msg = ChatMsg {
        id: g.chat_seq,
        tick: g.tick,
        pid: 0,
        name: "System".into(),
        text: text.into(),
    };
    if let Some(p) = g.players.get_mut(&pid) {
        p.private_chat.push(msg);
    }
}

fn add_chat(g: &mut Game, pid: Pid, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let clean: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
    if clean.is_empty() {
        return;
    }

    if clean == "/help" {
        push_private_chat(g, pid, "Commands: /help, /nick name");
        push_private_chat(g, pid, "Controls: left click to walk, chop, mine, fish (rod + water), attack, trade. Right click to stop.");
        return;
    }
    if let Some(rest) = clean.strip_prefix("/nick ") {
        let new_name = clean_name(rest);
        let old_name = match g.players.get_mut(&pid) {
            Some(p) => {
                let old = p.name.clone();
                p.name = new_name.clone();
                old
            }
            None => return,
        };
        g.chat_seq += 1;
        g.chat.push_back(ChatMsg {
            id: g.chat_seq,
            tick: g.tick,
            pid: 0,
            name: "System".into(),
            text: format!("{} is now known as {}.", old_name, new_name),
        });
        while g.chat.len() > 50 {
            g.chat.pop_front();
        }
        return;
    }
    if clean.starts_with('/') {
        push_private_chat(g, pid, "Unknown command. Try /help.");
        return;
    }

    let name = g
        .players
        .get(&pid)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "anon".into());
    g.chat_seq += 1;
    g.chat.push_back(ChatMsg {
        id: g.chat_seq,
        tick: g.tick,
        pid,
        name,
        text: clean,
    });
    while g.chat.len() > 50 {
        g.chat.pop_front();
    }
}
fn eat(g: &mut Game, pid: Pid, slot: usize) {
    let p = g.players.get_mut(&pid).unwrap();
    if slot >= INV_SIZE || p.inv[slot].qty <= 0 {
        return;
    }
    if p.hp_cur >= p.skills.hp {
        p.log.push("You're already at full health.".into());
        return;
    }
    let (amt, msg) = match p.inv[slot].item.as_str() {
        "berries" => (3, "You eat the berries. (+3 HP)"),
        "salmon" => (30, "You eat the salmon. (+30 HP)"),
        _ => return,
    };
    p.inv[slot].qty -= 1;
    if p.inv[slot].qty == 0 {
        p.inv[slot] = InvSlot::default();
    }
    p.hp_cur = (p.hp_cur + amt).min(p.skills.hp);
    p.log.push(msg.into());
}

fn roll_hit(atk: i32, def: i32, str_: i32) -> i32 {
    let acc = (atk as f32 + 8.0) / (atk as f32 + def as f32 + 16.0);
    if rand_f() < acc {
        let max = 1 + str_ / 4;
        1 + rand_range(max)
    } else {
        0
    }
}

fn mob_name(kind: &str) -> &'static str {
    mob_def(kind).name
}

fn mob_aggro_radius(kind: &str) -> i32 {
    mob_def(kind).aggro
}

fn process_player(g: &mut Game, pid: Pid) {
    let (pos, intent, target) = {
        let p = match g.players.get(&pid) {
            Some(p) => p,
            None => return,
        };
        ((p.x, p.y), p.intent, p.target)
    };
    let target = match intent {
        Intent::Attack { mid } => match g.mobs.get(&mid) {
            Some(m) if m.respawn_at.is_none() => Some((m.x, m.y)),
            _ => {
                let p = g.players.get_mut(&pid).unwrap();
                p.intent = Intent::None;
                p.target = None;
                return;
            }
        },
        Intent::Trade { pid: other_pid } => match g.players.get(&other_pid) {
            Some(other) => Some((other.x, other.y)),
            None => {
                let p = g.players.get_mut(&pid).unwrap();
                p.intent = Intent::None;
                p.target = None;
                p.log.push("They are no longer here.".into());
                return;
            }
        },
        _ => target,
    };
    let Some(t) = target else {
        return;
    };
    let needs_adj = !matches!(intent, Intent::None | Intent::Pickup);
    let at_goal = if needs_adj {
        chebyshev(pos, t) == 1
    } else {
        pos == t
    };
    if at_goal {
        match intent {
            Intent::Chop => do_chop(g, pid, t),
            Intent::Mine => do_mine(g, pid, t),
            Intent::Pick => do_pick(g, pid, t),
            Intent::Fish => do_fish(g, pid, t),
            Intent::Pickup => do_pickup(g, pid, t),
            Intent::Attack { mid } => do_attack(g, pid, mid),
            Intent::Trade { pid: other_pid } => {
                request_player_trade(g, pid, other_pid);
                if let Some(p) = g.players.get_mut(&pid) {
                    p.intent = Intent::None;
                    p.target = None;
                }
            }
            Intent::Talk => {
                let talk_obj = g.obj(t.0, t.1).clone();
                let p = g.players.get_mut(&pid).unwrap();
                match talk_obj {
                    Obj::Trader => {
                        p.trade_open = true;
                        p.angel_modal_open = false;
                    }
                    Obj::Angel => {
                        p.angel_modal_open = true;
                        p.trade_open = false;
                    }
                    _ => {}
                }
                p.intent = Intent::None;
                p.target = None;
            }
            Intent::None => {}
        }
        return;
    }
    let goal_kind = if needs_adj {
        GoalKind::Adjacent(t)
    } else {
        GoalKind::Step(t)
    };
    if let Some(mut path) = bfs(g, pos, goal_kind, pid) {
        if let Some(step) = path.pop_front() {
            if g.walkable(step.0, step.1, pid) {
                let p = g.players.get_mut(&pid).unwrap();
                p.x = step.0;
                p.y = step.1;
            }
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
        p.log.push("Can't reach there.".into());
    }
}

fn do_chop(g: &mut Game, pid: Pid, t: (i32, i32)) {
    let (tool, level) = {
        let p = g.players.get(&pid).unwrap();
        (best_tool(p, "axe"), p.skills.woodcutting)
    };
    let Some(tool) = tool else {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("You need an axe.".into());
        p.intent = Intent::None;
        p.target = None;
        return;
    };
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Tree { tier, hp } = obj {
        let def = tree_def(tier);
        if tool.tier < def.req_tool_tier {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!(
                "You need a tier {} axe for {}.",
                def.req_tool_tier, def.name
            ));
            p.intent = Intent::None;
            p.target = None;
            return;
        }
        g.events.push(json!({"k":"chop","x":t.0,"y":t.1}));
        if !gather_success(level, tier) {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!("You fail to cut the {}.", def.name));
            return;
        }
        let dmg = tool.power + level / 12;
        let new_hp = hp - dmg;
        if new_hp <= 0 {
            g.set_obj(
                t.0,
                t.1,
                Obj::Stump {
                    tier,
                    regrow: g.tick + def.regrow_secs * TPS,
                },
            );
            let p = g.players.get_mut(&pid).unwrap();
            if add_inv(p, def.item, 1) {
                p.log.push(format!("You get {}.", item_name(def.item)));
            } else {
                p.log.push("Inventory full!".into());
            }
            let ap = p.skills.angel_points;
            p.skills.woodcutting_xp += xp_with_bonus(def.xp, ap);
            p.skills.woodcutting = level_from_xp(p.skills.woodcutting_xp);
            log_level_up(&mut p.log, "Woodcutting", level, p.skills.woodcutting);
            p.intent = Intent::None;
            p.target = None;
        } else {
            g.set_obj(t.0, t.1, Obj::Tree { tier, hp: new_hp });
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
    }
}
fn do_mine(g: &mut Game, pid: Pid, t: (i32, i32)) {
    let (tool, level) = {
        let p = g.players.get(&pid).unwrap();
        (best_tool(p, "pickaxe"), p.skills.mining)
    };
    let Some(tool) = tool else {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("You need a pickaxe.".into());
        p.intent = Intent::None;
        p.target = None;
        return;
    };
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Rock { tier, hp } = obj {
        let def = rock_def(tier);
        if tool.tier < def.req_tool_tier {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!(
                "You need a tier {} pickaxe for {}.",
                def.req_tool_tier, def.name
            ));
            p.intent = Intent::None;
            p.target = None;
            return;
        }
        g.events.push(json!({"k":"mine","x":t.0,"y":t.1}));
        if !gather_success(level, tier) {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!("You fail to mine the {}.", def.name));
            return;
        }
        let dmg = tool.power + level / 12;
        let new_hp = hp - dmg;
        if new_hp <= 0 {
            g.set_obj(
                t.0,
                t.1,
                Obj::DepletedRock {
                    tier,
                    regrow: g.tick + def.regrow_secs * TPS,
                },
            );
            let p = g.players.get_mut(&pid).unwrap();
            if add_inv(p, def.item, 1) {
                p.log.push(format!("You mine {}.", item_name(def.item)));
            } else {
                p.log.push("Inventory full!".into());
            }
            let ap = p.skills.angel_points;
            p.skills.mining_xp += xp_with_bonus(def.xp, ap);
            p.skills.mining = level_from_xp(p.skills.mining_xp);
            log_level_up(&mut p.log, "Mining", level, p.skills.mining);
            p.intent = Intent::None;
            p.target = None;
        } else {
            g.set_obj(t.0, t.1, Obj::Rock { tier, hp: new_hp });
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
    }
}
fn do_fish(g: &mut Game, pid: Pid, t: (i32, i32)) {
    let (has_rod, level) = {
        let p = g.players.get(&pid).unwrap();
        (has_item(p, "fishing_rod"), p.skills.fishing)
    };
    if !has_rod {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("You need a fishing rod.".into());
        p.intent = Intent::None;
        p.target = None;
        return;
    }
    if !matches!(g.tile(t.0, t.1), Tile::Water) {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
        return;
    }
    g.events.push(json!({"k":"fish","x":t.0,"y":t.1}));
    let bite = fish_bite_chance(level);
    if rand_f() >= bite {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("The fish slips away.".into());
        return;
    }
    let p = g.players.get_mut(&pid).unwrap();
    if !add_inv(p, "salmon", 1) {
        p.log.push("Inventory full!".into());
        p.intent = Intent::None;
        p.target = None;
        return;
    }
    p.log.push("You catch a salmon.".into());
    let ap = p.skills.angel_points;
    p.skills.fishing_xp += xp_with_bonus(42, ap);
    let old_fishing = p.skills.fishing;
    p.skills.fishing = level_from_xp(p.skills.fishing_xp);
    log_level_up(&mut p.log, "Fishing", old_fishing, p.skills.fishing);
    p.intent = Intent::None;
    p.target = None;
}

fn do_pick(g: &mut Game, pid: Pid, t: (i32, i32)) {
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Bush { berries, .. } = obj {
        if berries > 0 {
            let new_berries = berries - 1;
            g.set_obj(
                t.0,
                t.1,
                Obj::Bush {
                    berries: new_berries,
                    regrow: g.tick + 8 * TPS,
                },
            );
            g.events.push(json!({"k":"pick","x":t.0,"y":t.1}));
            let p = g.players.get_mut(&pid).unwrap();
            if !add_inv(p, "berries", 1) {
                p.log.push("Inventory full!".into());
                p.intent = Intent::None;
                p.target = None;
                return;
            }
            p.log.push("You pick a berry.".into());
            if new_berries == 0 {
                p.intent = Intent::None;
                p.target = None;
            }
        } else {
            let p = g.players.get_mut(&pid).unwrap();
            p.intent = Intent::None;
            p.target = None;
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
    }
}
fn do_pickup(g: &mut Game, pid: Pid, t: (i32, i32)) {
    let mut taken: Vec<GroundItem> = Vec::new();
    let mut i = 0;
    while i < g.ground.len() {
        if g.ground[i].x == t.0 && g.ground[i].y == t.1 {
            taken.push(g.ground.remove(i));
        } else {
            i += 1;
        }
    }
    {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None;
        p.target = None;
    }
    if taken.is_empty() {
        return;
    }
    let mut full = false;
    for gi in taken {
        let added = if full {
            false
        } else {
            let p = g.players.get_mut(&pid).unwrap();
            add_inv(p, &gi.item, gi.qty)
        };
        if !added {
            full = true;
            g.ground.push(gi);
            continue;
        }
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push(format!("You pick up {}.", item_name(&gi.item)));
    }
    if full {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("Inventory full!".into());
    }
}

fn drop_one(g: &mut Game, pid: Pid, slot: usize) {
    let Some(p) = g.players.get_mut(&pid) else {
        return;
    };
    if slot >= INV_SIZE {
        return;
    }
    let s = &mut p.inv[slot];
    if s.item.is_empty() || s.qty <= 0 {
        return;
    }
    let item = s.item.clone();
    s.qty -= 1;
    if s.qty == 0 {
        *s = InvSlot::default();
    }
    let (x, y) = (p.x, p.y);
    p.log.push(format!("You drop {}.", item_name(&item)));
    g.drop_ground(x, y, &item, 1);
}

fn equip_from_inv(g: &mut Game, pid: Pid, slot: usize) {
    let Some(p) = g.players.get_mut(&pid) else {
        return;
    };
    if slot >= INV_SIZE {
        return;
    }
    let item = p.inv[slot].item.clone();
    if item.is_empty() {
        return;
    }
    let Some(eq_slot) = item_equip_slot(&item) else {
        p.log.push(format!("{} cannot be equipped.", item_name(&item)));
        return;
    };
    let qty = p.inv[slot].qty;
    let prev = p.equipment.get(eq_slot).to_string();
    if qty > 1 {
        p.inv[slot].qty -= 1;
    } else {
        p.inv[slot] = InvSlot::default();
    }
    p.equipment.set(eq_slot, item.clone());
    if !prev.is_empty() {
        if !add_inv(p, &prev, 1) {
            // No room — drop it on the floor instead of losing it.
            let (x, y) = (p.x, p.y);
            g.drop_ground(x, y, &prev, 1);
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!("{} falls to the ground.", item_name(&prev)));
        }
    }
    let p = g.players.get_mut(&pid).unwrap();
    p.log.push(format!("You equip {}.", item_name(&item)));
}

fn unequip_slot(g: &mut Game, pid: Pid, eq_slot: &str) {
    if !EQUIP_SLOT_NAMES.contains(&eq_slot) {
        return;
    }
    let Some(p) = g.players.get_mut(&pid) else {
        return;
    };
    let item = p.equipment.get(eq_slot).to_string();
    if item.is_empty() {
        return;
    }
    p.equipment.set(eq_slot, String::new());
    if !add_inv(p, &item, 1) {
        // No room — drop on floor.
        let (x, y) = (p.x, p.y);
        g.drop_ground(x, y, &item, 1);
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push(format!("{} falls to the ground.", item_name(&item)));
        return;
    }
    p.log.push(format!("You unequip {}.", item_name(&item)));
}

fn do_attack(g: &mut Game, pid: Pid, mid: Mid) {
    let (atk, str_) = {
        let p = g.players.get(&pid).unwrap();
        (p.skills.attack, p.skills.strength)
    };
    let (mdef, mx, my, kind) = match g.mobs.get(&mid) {
        Some(m) => (m.defence, m.x, m.y, m.kind.clone()),
        _ => return,
    };
    let dmg = roll_hit(atk, mdef, str_);
    let killed = {
        let m = g.mobs.get_mut(&mid).unwrap();
        m.hp_cur = (m.hp_cur - dmg).max(0);
        m.hp_cur == 0
    };
    g.events.push(
        json!({"k": if dmg == 0 { "miss_mob" } else { "hit_mob" }, "x": mx, "y": my, "dmg": dmg}),
    );
    let p = g.players.get_mut(&pid).unwrap();
    p.log.push(if dmg == 0 {
        format!("You miss the {}.", mob_name(&kind))
    } else {
        format!("You hit the {} for {}.", mob_name(&kind), dmg)
    });
    let ap = p.skills.angel_points;
    p.skills.attack_xp += xp_with_bonus(8 + dmg * 4, ap);
    let old_attack = p.skills.attack;
    p.skills.attack = level_from_xp(p.skills.attack_xp);
    p.skills.strength_xp += xp_with_bonus(dmg * 4, ap);
    let old_strength = p.skills.strength;
    p.skills.strength = level_from_xp(p.skills.strength_xp);
    log_level_up(&mut p.log, "Attack", old_attack, p.skills.attack);
    log_level_up(&mut p.log, "Strength", old_strength, p.skills.strength);
    if killed {
        p.log.push(format!("You kill the {}!", mob_name(&kind)));
        p.intent = Intent::None;
        p.target = None;
        let def = mob_def(&kind);
        let drops = roll_mob_drops(def.tier);
        let (mx, my) = {
            let m = g.mobs.get_mut(&mid).unwrap();
            m.respawn_at = Some(g.tick + 20 * TPS);
            (m.x, m.y)
        };
        g.drop_ground(mx, my, "coins", def.coin);
        for (item, qty) in drops {
            g.drop_ground(mx, my, item, qty);
            if let Some(p) = g.players.get_mut(&pid) {
                p.log.push(format!("The {} drops {}!", mob_name(&kind), item_name(item)));
            }
        }
    }
}

fn process_mob(g: &mut Game, mid: Mid) {
    let (pos, respawning, home, kind) = {
        let m = match g.mobs.get(&mid) {
            Some(m) => m,
            None => return,
        };
        ((m.x, m.y), m.respawn_at, m.home, m.kind.clone())
    };
    if let Some(t) = respawning {
        if g.tick >= t {
            let m = g.mobs.get_mut(&mid).unwrap();
            m.x = home.0;
            m.y = home.1;
            m.hp_cur = m.hp_max;
            m.respawn_at = None;
        }
        return;
    }
    let aggro = mob_aggro_radius(&kind);
    let target = g
        .players
        .values()
        .filter(|p| manhattan((p.x, p.y), pos) <= aggro)
        .min_by_key(|p| manhattan((p.x, p.y), pos))
        .map(|p| (p.id, p.x, p.y));
    let Some((tpid, tx, ty)) = target else {
        return;
    };
    if chebyshev(pos, (tx, ty)) == 1 {
        let (matk, mstr) = {
            let m = g.mobs.get(&mid).unwrap();
            (m.attack, m.strength)
        };
        let pdef = g.players.get(&tpid).unwrap().skills.defence;
        let dmg = roll_hit(matk, pdef, mstr);
        g.events.push(json!({"k": if dmg == 0 { "miss_player" } else { "hit_player" }, "x": tx, "y": ty, "dmg": dmg}));
        let spawn = g.player_spawn;
        let p = g.players.get_mut(&tpid).unwrap();
        p.hp_cur = (p.hp_cur - dmg).max(0);
        p.log.push(if dmg == 0 {
            format!("The {} misses you.", mob_name(&kind))
        } else {
            format!("The {} hits you for {}.", mob_name(&kind), dmg)
        });
        let ap = p.skills.angel_points;
        if dmg == 0 {
            p.skills.defence_xp += xp_with_bonus(8, ap);
            let old_defence = p.skills.defence;
            p.skills.defence = level_from_xp(p.skills.defence_xp);
            log_level_up(&mut p.log, "Defence", old_defence, p.skills.defence);
        } else {
            p.skills.hp_xp += xp_with_bonus(dmg, ap);
            let old_hp = p.skills.hp;
            p.skills.hp = 10 + level_from_xp(p.skills.hp_xp) - 1;
            log_level_up(&mut p.log, "HP", old_hp, p.skills.hp);
        }
        if matches!(p.intent, Intent::None) && p.hp_cur > 0 {
            p.intent = Intent::Attack { mid };
            p.target = Some((pos.0, pos.1));
        }
        if p.hp_cur == 0 {
            p.log.push("You die! Respawning...".into());
            p.hp_cur = p.skills.hp;
            p.x = spawn.0;
            p.y = spawn.1;
            p.intent = Intent::None;
            p.target = None;
            p.trade_open = false;
            p.angel_modal_open = false;
        }
    } else if let Some(mut path) = bfs(g, pos, GoalKind::Adjacent((tx, ty)), 0) {
        if let Some(step) = path.pop_front() {
            if g.walkable(step.0, step.1, 0) {
                let m = g.mobs.get_mut(&mid).unwrap();
                m.x = step.0;
                m.y = step.1;
            }
        }
    }
}

fn tick_world(g: &mut Game) {
    g.tick += 1;
    let now = g.tick;
    for i in 0..g.objects.len() {
        let new = match &g.objects[i] {
            Obj::Stump { tier, regrow } if *regrow <= now => Some(Obj::Tree {
                tier: *tier,
                hp: tree_def(*tier).hp,
            }),
            Obj::DepletedRock { tier, regrow } if *regrow <= now => Some(Obj::Rock {
                tier: *tier,
                hp: rock_def(*tier).hp,
            }),
            Obj::Bush { berries, regrow } if *berries < 3 && *regrow <= now => Some(Obj::Bush {
                berries: berries + 1,
                regrow: now + 8 * TPS,
            }),
            _ => None,
        };
        if let Some(o) = new {
            g.objects[i] = o;
        }
    }
    let pids: Vec<_> = g.players.keys().copied().collect();
    for pid in &pids {
        process_player(g, *pid);
    }
    let mids: Vec<_> = g.mobs.keys().copied().collect();
    for mid in &mids {
        process_mob(g, *mid);
    }
    for p in g.players.values_mut() {
        p.regen_ctr += 1;
        if p.regen_ctr as u64 >= 6 * TPS {
            p.regen_ctr = 0;
            if p.hp_cur < p.skills.hp {
                p.hp_cur += 1;
            }
        }
    }
}

fn shop_catalog() -> Vec<Value> {
    TOOL_DEFS
        .iter()
        .map(|t| {
            json!({
                "item": t.item,
                "name": t.name,
                "kind": t.kind,
                "tier": t.tier,
                "buy": t.buy,
            })
        })
        .collect()
}

fn sell_catalog() -> Vec<Value> {
    let mut out: Vec<Value> = TOOL_DEFS
        .iter()
        .map(|t| {
            json!({
                "item": t.item,
                "name": t.name,
                "tier": t.tier,
                "sell": t.buy,
            })
        })
        .collect();
    out.extend(TREE_DEFS.iter().map(|r| {
        json!({
            "item": r.item,
            "name": r.name,
            "tier": r.tier,
            "sell": r.sell,
            "xp": r.xp,
        })
    }));
    out.extend(ROCK_DEFS.iter().map(|r| {
        json!({
            "item": r.item,
            "name": r.name,
            "tier": r.tier,
            "sell": r.sell,
            "xp": r.xp,
        })
    }));
    out.push(json!({ "item": "berries", "name": "Berries", "tier": 1, "sell": 1, "xp": 0 }));
    out.push(json!({ "item": "salmon", "name": "Salmon", "tier": 2, "sell": 35, "xp": 42 }));
    out
}

fn trade_offer_json(p: &Player) -> Vec<Value> {
    offer_items_from_slots(&p.inv, &p.trade_offer)
        .unwrap_or_default()
        .into_iter()
        .map(|(slot, it)| json!({ "slot": slot, "item": it.item, "qty": it.qty, "name": item_name(&it.item) }))
        .collect()
}

fn build_state_msg(g: &Game, pid: Pid) -> String {
    let p = &g.players[&pid];
    let axe_tier = best_tool(p, "axe").map(|t| t.tier).unwrap_or(0);
    let pickaxe_tier = best_tool(p, "pickaxe").map(|t| t.tier).unwrap_or(0);
    let rod_tier = best_tool(p, "rod").map(|t| t.tier).unwrap_or(0);
    let players: Vec<Value> = g.players.values().map(|q| json!({
        "id": q.id, "x": q.x, "y": q.y, "name": q.name, "hp": q.hp_cur, "hp_max": q.skills.hp
    })).collect();
    let mobs: Vec<Value> = g
        .mobs
        .values()
        .filter(|m| m.respawn_at.is_none())
        .map(|m| {
            json!({
                "id": m.id, "kind": m.kind, "x": m.x, "y": m.y, "hp": m.hp_cur, "hp_max": m.hp_max
            })
        })
        .collect();
    let mut chat: Vec<ChatMsg> = g.chat.iter().cloned().collect();
    chat.extend(p.private_chat.iter().cloned());
    let player_trade = p.trade_partner.and_then(|partner_id| {
        let other = g.players.get(&partner_id)?;
        if other.trade_partner != Some(pid) {
            return None;
        }
        Some(json!({
            "open": true,
            "partner_id": other.id,
            "partner_name": other.name,
            "stage": p.trade_stage,
            "your_offer_slots": p.trade_offer.clone(),
            "your_offer": trade_offer_json(p),
            "their_offer": trade_offer_json(other),
            "your_accepted": p.trade_accepted,
            "their_accepted": other.trade_accepted,
            "your_confirmed": p.trade_confirmed,
            "their_confirmed": other.trade_confirmed,
        }))
    });
    json!({
        "t": "state", "tick": g.tick, "tick_ms": TICK_MS,
        "you": { "id": p.id, "x": p.x, "y": p.y, "hp": p.hp_cur, "skills": p.skills,
                 "inv": p.inv, "equipment": p.equipment,
                 "axe_tier": axe_tier, "pickaxe_tier": pickaxe_tier, "rod_tier": rod_tier,
                 "intent": p.intent, "target": p.target, "trade_open": p.trade_open && near_trader(g, pid),
                 "angel_modal_open": p.angel_modal_open && near_angel(g, pid) },
        "players": players, "mobs": mobs, "objects": g.objects, "ground": g.ground, "log": p.log,
        "shop": shop_catalog(), "sells": sell_catalog(),
        "player_trade": player_trade,
        "chat": chat,
        "events": g.events,
    }).to_string()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<Mutex<Game>>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |s| handle_socket(s, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<Mutex<Game>>) {
    let (mut sink, mut stream) = socket.split();
    let join = match stream.next().await {
        Some(Ok(Message::Text(t))) => t,
        _ => return,
    };
    let v: Value = match serde_json::from_str(&join) {
        Ok(v) => v,
        _ => return,
    };
    if v.get("t").and_then(|x| x.as_str()) != Some("join") {
        return;
    }
    let requested_name = clean_name(
        v.get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("Adventurer"),
    );
    let uuid = valid_uuid_or_new(v.get("uuid").and_then(|x| x.as_str()));
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let pid = new_pid();
    {
        let mut g = state.lock().await;
        let saved = load_player(&g.db, &uuid);
        let mut p = if let Some(saved) = saved {
            let mut p = Player::new(
                pid,
                uuid.clone(),
                clean_name(&saved.name),
                saved.x,
                saved.y,
                tx.clone(),
            );
            p.hp_cur = saved.hp_cur.clamp(1, saved.skills.hp);
            p.skills = saved.skills;
            p.inv = saved.inv;
            p.equipment = saved.equipment;
            p
        } else {
            Player::new(
                pid,
                uuid.clone(),
                requested_name,
                g.player_spawn.0,
                g.player_spawn.1,
                tx.clone(),
            )
        };
        p.log
            .push("Welcome to Tradscape, /help for commands.".into());
        let init =
            json!({ "t": "init", "w": g.w, "h": g.h, "tiles": g.tiles, "you": pid, "uuid": uuid })
                .to_string();
        let _ = tx.send(init);
        let name = p.name.clone();
        g.players.insert(pid, p);
        println!("Player {} ({}) joined", pid, name);
    }
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(text) = msg {
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                _ => continue,
            };
            let save = {
                let mut g = state.lock().await;
                let mut push_state = false;
                match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                    "click" => {
                        let x = v.get("x").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        let y = v.get("y").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        click(&mut g, pid, x, y);
                    }
                    "attack" => {
                        let mid = v.get("mid").and_then(|x| x.as_u64()).unwrap_or(0);
                        attack(&mut g, pid, mid);
                    }
                    "trade_player" => {
                        let other_pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
                        trade(&mut g, pid, other_pid);
                    }
                    "stop" => {
                        cancel_player_trade(&mut g, pid, "Trade cancelled.");
                        if let Some(p) = g.players.get_mut(&pid) {
                            p.intent = Intent::None;
                            p.target = None;
                            p.trade_open = false;
                            p.angel_modal_open = false;
                        }
                    }
                    "eat" => {
                        let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        eat(&mut g, pid, s);
                        push_state = true;
                    }
                    "drop" => {
                        let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        drop_one(&mut g, pid, s);
                        push_state = true;
                    }
                    "equip" => {
                        let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        equip_from_inv(&mut g, pid, s);
                        push_state = true;
                    }
                    "unequip" => {
                        let slot = v.get("slot").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        unequip_slot(&mut g, pid, &slot);
                        push_state = true;
                    }
                    "buy" => {
                        let item = v
                            .get("item")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        buy(&mut g, pid, &item);
                        push_state = true;
                    }
                    "sell" => {
                        let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        sell(&mut g, pid, s);
                        push_state = true;
                    }
                    "close_trade" => {
                        if let Some(p) = g.players.get_mut(&pid) {
                            p.trade_open = false;
                        }
                    }
                    "close_player_trade" => {
                        cancel_player_trade(&mut g, pid, "Trade declined.");
                    }
                    "trade_offer_slot" => {
                        let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        trade_offer_slot(&mut g, pid, s);
                    }
                    "trade_accept" => {
                        trade_accept(&mut g, pid);
                    }
                    "trade_confirm" => {
                        trade_confirm(&mut g, pid);
                    }
                    "angel_confirm" => {
                        angel_sacrifice(&mut g, pid);
                    }
                    "angel_decline" => {
                        angel_decline(&mut g, pid);
                    }
                    "chat" => {
                        let text = v
                            .get("text")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        add_chat(&mut g, pid, &text);
                    }
                    _ => {}
                }
                if push_state && g.players.contains_key(&pid) {
                    let msg = build_state_msg(&g, pid);
                    if let Some(p) = g.players.get_mut(&pid) {
                        let _ = p.tx.send(msg);
                        p.log.clear();
                        p.private_chat.clear();
                    }
                }
                g.players.get(&pid).map(PlayerSave::from)
            };
            if let Some(save) = save {
                save_player_without_game_lock(&save);
            }
        }
    }
    let save = {
        let mut g = state.lock().await;
        let save = g.players.get(&pid).map(PlayerSave::from);
        cancel_player_trade(&mut g, pid, "Other player disconnected.");
        g.players.remove(&pid);
        println!("Player {} left", pid);
        save
    };
    if let Some(save) = save {
        save_player_without_game_lock(&save);
    }
    send_task.abort();
}

fn resolve_static_root() -> PathBuf {
    if let Ok(p) = std::env::var("TRADSCAPE_ROOT") {
        return PathBuf::from(p);
    }
    let has_client =
        |dir: &PathBuf| dir.join("index.html").exists() || dir.join("tradscape.html").exists();
    if let Ok(cwd) = std::env::current_dir() {
        if has_client(&cwd) {
            return cwd;
        }
        let parent = cwd.join("..");
        if has_client(&parent) {
            return parent.canonicalize().unwrap_or(parent);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[tokio::main]
async fn main() {
    let state = Arc::new(Mutex::new(Game::new()));
    let s2 = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(TICK_MS));
        loop {
            interval.tick().await;
            let mut g = s2.lock().await;
            tick_world(&mut g);
            let pids: Vec<_> = g.players.keys().copied().collect();
            for pid in pids {
                let msg = build_state_msg(&g, pid);
                if let Some(p) = g.players.get_mut(&pid) {
                    let _ = p.tx.send(msg);
                    p.log.clear();
                    p.private_chat.clear();
                }
            }
            if g.tick % (10 * TPS) == 0 {
                for p in g.players.values() {
                    save_player(&g.db, p);
                }
            }
            g.events.clear();
        }
    });
    let static_dir = resolve_static_root();
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route(
            "/tradscape.html",
            get(|| async { Redirect::temporary("/") }),
        )
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .with_state(state);
    let listener = TcpListener::bind("0.0.0.0:8081").await.unwrap();
    println!("Tradscape listening on http://localhost:8081");
    axum::serve(listener, app).await.unwrap();
}
