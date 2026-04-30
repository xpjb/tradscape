use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{atomic::{AtomicU64, Ordering}, Arc},
    time::Duration,
};
use tokio::{net::TcpListener, sync::{mpsc, Mutex}};
use tower_http::services::ServeDir;

const TICK_MS: u64 = 200;
const TPS: u64 = 1000 / TICK_MS;
const INV_SIZE: usize = 28;

type Pid = u64;
type Mid = u64;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
fn new_pid() -> Pid { NEXT_PID.fetch_add(1, Ordering::Relaxed) }

static RNG: AtomicU64 = AtomicU64::new(0xdead_beef_cafe_babe);
fn rand_u64() -> u64 {
    let mut x = RNG.load(Ordering::Relaxed);
    if x == 0 { x = 1; }
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    RNG.store(x, Ordering::Relaxed);
    x
}
fn rand_f() -> f32 { (rand_u64() as f64 / u64::MAX as f64) as f32 }
fn rand_range(n: i32) -> i32 { if n <= 0 { 0 } else { (rand_u64() % n as u64) as i32 } }

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Tile { Grass, Dirt, Sand, Water, Stone, Path }

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
}

#[derive(Clone, Copy, Serialize, PartialEq)]
#[serde(tag = "k", rename_all = "snake_case")]
enum Intent { None, Chop, Mine, Pick, Talk, Attack { mid: Mid } }

#[derive(Clone, Serialize, Default)]
struct Skills {
    woodcutting: i32, mining: i32, attack: i32, strength: i32, defence: i32, hp: i32,
    woodcutting_xp: i32, mining_xp: i32, attack_xp: i32, strength_xp: i32, defence_xp: i32, hp_xp: i32,
}
impl Skills {
    fn starter() -> Self {
        Self { woodcutting:1, mining:1, attack:1, strength:1, defence:1, hp:10, ..Default::default() }
    }
}

#[derive(Clone, Serialize, Default)]
struct InvSlot { item: String, qty: i32 }

struct Player {
    id: Pid,
    name: String,
    x: i32, y: i32,
    hp_cur: i32,
    skills: Skills,
    inv: Vec<InvSlot>,
    target: Option<(i32,i32)>,
    intent: Intent,
    log: Vec<String>,
    tx: mpsc::UnboundedSender<String>,
    regen_ctr: u32,
}
impl Player {
    fn new(id: Pid, name: String, x: i32, y: i32, tx: mpsc::UnboundedSender<String>) -> Self {
        let mut inv = vec![InvSlot::default(); INV_SIZE];
        inv[0] = InvSlot { item: "bronze_axe".into(), qty: 1 };
        inv[1] = InvSlot { item: "bronze_pickaxe".into(), qty: 1 };
        Self { id, name, x, y, hp_cur: 10, skills: Skills::starter(), inv,
               target: None, intent: Intent::None,
               log: vec![], tx, regen_ctr: 0 }
    }
}

struct Mob {
    id: Mid,
    kind: String,
    x: i32, y: i32,
    hp_cur: i32, hp_max: i32,
    attack: i32, strength: i32, defence: i32,
    home: (i32,i32),
    respawn_at: Option<u64>,
}

struct Game {
    w: i32, h: i32,
    tiles: Vec<Tile>,
    objects: Vec<Obj>,
    players: HashMap<Pid, Player>,
    mobs: HashMap<Mid, Mob>,
    tick: u64,
    next_mid: Mid,
    events: Vec<Value>,
}

impl Game {
    fn new() -> Self {
        let (w, h, tiles, objects) = build_map();
        let mut g = Self { w, h, tiles, objects, players: HashMap::new(), mobs: HashMap::new(), tick: 0, next_mid: 1, events: Vec::new() };
        g.spawn_mob("goblin", 10, 10, 7, 2, 2, 1);
        g.spawn_mob("goblin", 30, 25, 7, 2, 2, 1);
        g.spawn_mob("goblin", 12, 28, 7, 2, 2, 1);
        g.spawn_mob("goblin", 25, 30, 12, 4, 4, 3);
        g
    }
    fn spawn_mob(&mut self, kind: &str, x: i32, y: i32, hp: i32, atk: i32, str_: i32, def: i32) {
        let id = self.next_mid; self.next_mid += 1;
        self.mobs.insert(id, Mob {
            id, kind: kind.into(), x, y, hp_cur: hp, hp_max: hp,
            attack: atk, strength: str_, defence: def, home: (x,y), respawn_at: None,
        });
    }
    fn idx(&self, x: i32, y: i32) -> usize { (y * self.w + x) as usize }
    fn in_b(&self, x: i32, y: i32) -> bool { x >= 0 && y >= 0 && x < self.w && y < self.h }
    fn tile(&self, x: i32, y: i32) -> Tile { self.tiles[self.idx(x, y)] }
    fn obj(&self, x: i32, y: i32) -> &Obj { &self.objects[self.idx(x, y)] }
    fn set_obj(&mut self, x: i32, y: i32, o: Obj) { let i = self.idx(x, y); self.objects[i] = o; }
    fn occupant_pid(&self, x: i32, y: i32) -> Option<Pid> {
        self.players.values().find(|p| p.x == x && p.y == y).map(|p| p.id)
    }
    fn occupant_mid(&self, x: i32, y: i32) -> Option<Mid> {
        self.mobs.values().find(|m| m.respawn_at.is_none() && m.x == x && m.y == y).map(|m| m.id)
    }
    fn walkable(&self, x: i32, y: i32, ignore_pid: Pid) -> bool {
        if !self.in_b(x, y) { return false; }
        if matches!(self.tile(x, y), Tile::Water) { return false; }
        if !matches!(self.obj(x, y), Obj::None | Obj::Stump{..} | Obj::DepletedRock{..}) { return false; }
        if let Some(pid) = self.occupant_pid(x, y) { if pid != ignore_pid { return false; } }
        if self.occupant_mid(x, y).is_some() { return false; }
        true
    }
}

fn manhattan(a: (i32,i32), b: (i32,i32)) -> i32 { (a.0 - b.0).abs() + (a.1 - b.1).abs() }
fn chebyshev(a: (i32,i32), b: (i32,i32)) -> i32 { (a.0 - b.0).abs().max((a.1 - b.1).abs()) }

#[derive(Clone, Copy)]
enum GoalKind { Step((i32,i32)), Adjacent((i32,i32)) }

fn bfs(g: &Game, from: (i32,i32), goal: GoalKind, ignore_pid: Pid) -> Option<VecDeque<(i32,i32)>> {
    let blocked = match goal { GoalKind::Adjacent(t) => Some(t), _ => None };
    let goal_test = |p: (i32,i32)| match goal {
        GoalKind::Step(t) => p == t,
        GoalKind::Adjacent(t) => chebyshev(p, t) == 1,
    };
    if goal_test(from) { return Some(VecDeque::new()); }
    let mut prev: HashMap<(i32,i32),(i32,i32)> = HashMap::new();
    let mut q = VecDeque::new();
    q.push_back(from); prev.insert(from, from);
    while let Some(cur) = q.pop_front() {
        for (dx, dy) in [(1,0),(-1,0),(0,1),(0,-1),(1,1),(1,-1),(-1,1),(-1,-1)] {
            let n = (cur.0 + dx, cur.1 + dy);
            if prev.contains_key(&n) { continue; }
            if Some(n) == blocked { continue; }
            if !g.walkable(n.0, n.1, ignore_pid) { continue; }
            // anti-corner-cut: diagonal moves require both orthogonal neighbors walkable
            if dx != 0 && dy != 0 {
                if !g.walkable(cur.0 + dx, cur.1, ignore_pid) { continue; }
                if !g.walkable(cur.0, cur.1 + dy, ignore_pid) { continue; }
            }
            prev.insert(n, cur);
            if goal_test(n) {
                let mut path = VecDeque::new();
                let mut c = n;
                while c != from { path.push_front(c); c = prev[&c]; }
                return Some(path);
            }
            q.push_back(n);
        }
    }
    None
}

fn level_from_xp(xp: i32) -> i32 { 1 + (xp / 50).min(98) }

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

const TREE_DEFS: [ResourceDef; 3] = [
    ResourceDef { tier: 1, name: "Pine Tree", item: "pine_logs", hp: 4, xp: 20, sell: 5, req_tool_tier: 1, regrow_secs: 25 },
    ResourceDef { tier: 2, name: "Oak Tree", item: "oak_logs", hp: 10, xp: 55, sell: 50, req_tool_tier: 2, regrow_secs: 45 },
    ResourceDef { tier: 3, name: "Yew Tree", item: "yew_logs", hp: 22, xp: 130, sell: 500, req_tool_tier: 3, regrow_secs: 75 },
];

const ROCK_DEFS: [ResourceDef; 3] = [
    ResourceDef { tier: 1, name: "Copper Rock", item: "copper_ore", hp: 5, xp: 25, sell: 6, req_tool_tier: 1, regrow_secs: 30 },
    ResourceDef { tier: 2, name: "Iron Rock", item: "iron_ore", hp: 12, xp: 65, sell: 60, req_tool_tier: 2, regrow_secs: 50 },
    ResourceDef { tier: 3, name: "Gold Rock", item: "gold_ore", hp: 26, xp: 150, sell: 600, req_tool_tier: 3, regrow_secs: 80 },
];

const TOOL_DEFS: [ToolDef; 6] = [
    ToolDef { item: "bronze_axe", name: "Bronze Axe", kind: "axe", tier: 1, buy: 10, power: 1 },
    ToolDef { item: "iron_axe", name: "Iron Axe", kind: "axe", tier: 2, buy: 200, power: 2 },
    ToolDef { item: "steel_axe", name: "Steel Axe", kind: "axe", tier: 3, buy: 4000, power: 4 },
    ToolDef { item: "bronze_pickaxe", name: "Bronze Pickaxe", kind: "pickaxe", tier: 1, buy: 10, power: 1 },
    ToolDef { item: "iron_pickaxe", name: "Iron Pickaxe", kind: "pickaxe", tier: 2, buy: 200, power: 2 },
    ToolDef { item: "steel_pickaxe", name: "Steel Pickaxe", kind: "pickaxe", tier: 3, buy: 4000, power: 4 },
];

fn tree_def(tier: i32) -> ResourceDef { TREE_DEFS[(tier - 1).clamp(0, 2) as usize] }
fn rock_def(tier: i32) -> ResourceDef { ROCK_DEFS[(tier - 1).clamp(0, 2) as usize] }

fn item_name(item: &str) -> &'static str {
    match item {
        "coins" => return "Coins",
        "berries" => return "Berries",
        "pine_logs" => return "Pine logs",
        "oak_logs" => return "Oak logs",
        "yew_logs" => return "Yew logs",
        "copper_ore" => return "Copper ore",
        "iron_ore" => return "Iron ore",
        "gold_ore" => return "Gold ore",
        _ => {}
    }
    if let Some(t) = TOOL_DEFS.iter().find(|t| t.item == item) { return t.name; }
    "Item"
}

fn sell_value(item: &str) -> Option<i32> {
    if item == "berries" { return Some(1); }
    TREE_DEFS.iter().chain(ROCK_DEFS.iter()).find(|r| r.item == item).map(|r| r.sell)
}

fn buy_price(item: &str) -> Option<i32> {
    TOOL_DEFS.iter().find(|t| t.item == item).map(|t| t.buy)
}

fn tool_def(item: &str) -> Option<ToolDef> {
    TOOL_DEFS.iter().find(|t| t.item == item).copied()
}

fn best_tool(p: &Player, kind: &str) -> Option<ToolDef> {
    p.inv.iter()
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

fn add_inv(p: &mut Player, item: &str, qty: i32) -> bool {
    for s in p.inv.iter_mut() {
        if s.item == item && s.qty > 0 { s.qty += qty; return true; }
    }
    for s in p.inv.iter_mut() {
        if s.item.is_empty() { s.item = item.into(); s.qty = qty; return true; }
    }
    false
}
fn coin_count(p: &Player) -> i32 { p.inv.iter().filter(|s| s.item == "coins").map(|s| s.qty).sum() }
fn deduct_coins(p: &mut Player, amt: i32) -> bool {
    if coin_count(p) < amt { return false; }
    let mut left = amt;
    for s in p.inv.iter_mut() {
        if s.item == "coins" {
            let take = left.min(s.qty);
            s.qty -= take; left -= take;
            if s.qty == 0 { *s = InvSlot::default(); }
            if left == 0 { return true; }
        }
    }
    true
}

fn click(g: &mut Game, pid: Pid, x: i32, y: i32) {
    if !g.in_b(x, y) { return; }
    if let Some(mid) = g.occupant_mid(x, y) {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::Attack { mid };
        p.target = Some((x, y));
        return;
    }
    let intent = match g.obj(x, y) {
        Obj::Tree {..} => Intent::Chop,
        Obj::Rock {..} => Intent::Mine,
        Obj::Bush { berries, .. } if *berries > 0 => Intent::Pick,
        Obj::Trader => Intent::Talk,
        _ => Intent::None,
    };
    let walk_ok = g.walkable(x, y, pid);
    let p = g.players.get_mut(&pid).unwrap();
    if matches!(intent, Intent::None) {
        if walk_ok { p.target = Some((x, y)); p.intent = Intent::None; }
        else { p.target = None; p.intent = Intent::None; }
    } else {
        p.target = Some((x, y));
        p.intent = intent;
    }
}

fn near_trader(g: &Game, pid: Pid) -> bool {
    let p = &g.players[&pid];
    for dy in -1..=1i32 { for dx in -1..=1i32 {
        let nx = p.x + dx; let ny = p.y + dy;
        if g.in_b(nx, ny) && matches!(g.obj(nx, ny), Obj::Trader) { return true; }
    }}
    false
}

fn buy(g: &mut Game, pid: Pid, item: &str) {
    if !near_trader(g, pid) {
        if let Some(p) = g.players.get_mut(&pid) { p.log.push("Stand next to the trader.".into()); }
        return;
    }
    let Some(price) = buy_price(item) else { return; };
    let p = g.players.get_mut(&pid).unwrap();
    if has_item(p, item) {
        p.log.push(format!("You already have a {}.", item_name(item)));
        return;
    }
    if !deduct_coins(p, price) { p.log.push("Not enough coins.".into()); return; }
    if add_inv(p, item, 1) {
        p.log.push(format!("You buy a {}.", item_name(item)));
    } else {
        add_inv(p, "coins", price);
        p.log.push("Inventory full!".into());
    }
}
fn sell(g: &mut Game, pid: Pid, slot: usize) {
    if !near_trader(g, pid) {
        if let Some(p) = g.players.get_mut(&pid) { p.log.push("Stand next to the trader.".into()); }
        return;
    }
    let p = g.players.get_mut(&pid).unwrap();
    if slot >= INV_SIZE { return; }
    let item = p.inv[slot].item.clone();
    if item.is_empty() || item == "coins" { return; }
    let Some(unit) = sell_value(&item) else {
        p.log.push("The trader does not buy that.".into());
        return;
    };
    let qty = p.inv[slot].qty;
    p.inv[slot] = InvSlot::default();
    add_inv(p, "coins", unit * qty);
    p.log.push(format!("Sold {}x{} for {}gp.", item, qty, unit * qty));
}
fn eat(g: &mut Game, pid: Pid, slot: usize) {
    let p = g.players.get_mut(&pid).unwrap();
    if slot >= INV_SIZE { return; }
    if p.inv[slot].item != "berries" || p.inv[slot].qty <= 0 { return; }
    p.inv[slot].qty -= 1;
    if p.inv[slot].qty == 0 { p.inv[slot] = InvSlot::default(); }
    p.hp_cur = (p.hp_cur + 3).min(p.skills.hp);
    p.log.push("You eat the berries. (+3 HP)".into());
}

fn roll_hit(atk: i32, def: i32, str_: i32) -> i32 {
    let acc = (atk as f32 + 8.0) / (atk as f32 + def as f32 + 16.0);
    if rand_f() < acc {
        let max = 1 + str_ / 4;
        1 + rand_range(max)
    } else { 0 }
}

fn process_player(g: &mut Game, pid: Pid) {
    let (pos, intent, target) = {
        let p = match g.players.get(&pid) { Some(p) => p, None => return };
        ((p.x, p.y), p.intent, p.target)
    };
    let target = match intent {
        Intent::Attack { mid } => match g.mobs.get(&mid) {
            Some(m) if m.respawn_at.is_none() => Some((m.x, m.y)),
            _ => {
                let p = g.players.get_mut(&pid).unwrap();
                p.intent = Intent::None; p.target = None;
                return;
            }
        },
        _ => target,
    };
    let Some(t) = target else { return; };
    let needs_adj = !matches!(intent, Intent::None);
    let at_goal = if needs_adj { chebyshev(pos, t) == 1 } else { pos == t };
    if at_goal {
        match intent {
            Intent::Chop => do_chop(g, pid, t),
            Intent::Mine => do_mine(g, pid, t),
            Intent::Pick => do_pick(g, pid, t),
            Intent::Attack { mid } => do_attack(g, pid, mid),
            Intent::Talk => {}
            Intent::None => {}
        }
        return;
    }
    let goal_kind = if needs_adj { GoalKind::Adjacent(t) } else { GoalKind::Step(t) };
    if let Some(mut path) = bfs(g, pos, goal_kind, pid) {
        if let Some(step) = path.pop_front() {
            if g.walkable(step.0, step.1, pid) {
                let p = g.players.get_mut(&pid).unwrap();
                p.x = step.0; p.y = step.1;
            }
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None; p.target = None;
        p.log.push("Can't reach there.".into());
    }
}

fn do_chop(g: &mut Game, pid: Pid, t: (i32,i32)) {
    let (tool, level) = {
        let p = g.players.get(&pid).unwrap();
        (best_tool(p, "axe"), p.skills.woodcutting)
    };
    let Some(tool) = tool else {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("You need an axe.".into());
        p.intent = Intent::None; p.target = None; return;
    };
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Tree { tier, hp } = obj {
        let def = tree_def(tier);
        if tool.tier < def.req_tool_tier {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!("You need a tier {} axe for {}.", def.req_tool_tier, def.name));
            p.intent = Intent::None; p.target = None; return;
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
            g.set_obj(t.0, t.1, Obj::Stump { tier, regrow: g.tick + def.regrow_secs * TPS });
            let p = g.players.get_mut(&pid).unwrap();
            if add_inv(p, def.item, 1) { p.log.push(format!("You get {}.", item_name(def.item))); }
            else { p.log.push("Inventory full!".into()); }
            p.skills.woodcutting_xp += def.xp;
            p.skills.woodcutting = level_from_xp(p.skills.woodcutting_xp);
            p.intent = Intent::None; p.target = None;
        } else {
            g.set_obj(t.0, t.1, Obj::Tree { tier, hp: new_hp });
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None; p.target = None;
    }
}
fn do_mine(g: &mut Game, pid: Pid, t: (i32,i32)) {
    let (tool, level) = {
        let p = g.players.get(&pid).unwrap();
        (best_tool(p, "pickaxe"), p.skills.mining)
    };
    let Some(tool) = tool else {
        let p = g.players.get_mut(&pid).unwrap();
        p.log.push("You need a pickaxe.".into());
        p.intent = Intent::None; p.target = None; return;
    };
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Rock { tier, hp } = obj {
        let def = rock_def(tier);
        if tool.tier < def.req_tool_tier {
            let p = g.players.get_mut(&pid).unwrap();
            p.log.push(format!("You need a tier {} pickaxe for {}.", def.req_tool_tier, def.name));
            p.intent = Intent::None; p.target = None; return;
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
            g.set_obj(t.0, t.1, Obj::DepletedRock { tier, regrow: g.tick + def.regrow_secs * TPS });
            let p = g.players.get_mut(&pid).unwrap();
            if add_inv(p, def.item, 1) { p.log.push(format!("You mine {}.", item_name(def.item))); }
            else { p.log.push("Inventory full!".into()); }
            p.skills.mining_xp += def.xp;
            p.skills.mining = level_from_xp(p.skills.mining_xp);
            p.intent = Intent::None; p.target = None;
        } else {
            g.set_obj(t.0, t.1, Obj::Rock { tier, hp: new_hp });
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None; p.target = None;
    }
}
fn do_pick(g: &mut Game, pid: Pid, t: (i32,i32)) {
    let obj = g.obj(t.0, t.1).clone();
    if let Obj::Bush { berries, .. } = obj {
        if berries > 0 {
            let new_berries = berries - 1;
            g.set_obj(t.0, t.1, Obj::Bush { berries: new_berries, regrow: g.tick + 8 * TPS });
            g.events.push(json!({"k":"pick","x":t.0,"y":t.1}));
            let p = g.players.get_mut(&pid).unwrap();
            if !add_inv(p, "berries", 1) {
                p.log.push("Inventory full!".into());
                p.intent = Intent::None; p.target = None;
                return;
            }
            p.log.push("You pick a berry.".into());
            if new_berries == 0 {
                p.intent = Intent::None; p.target = None;
            }
        } else {
            let p = g.players.get_mut(&pid).unwrap();
            p.intent = Intent::None; p.target = None;
        }
    } else {
        let p = g.players.get_mut(&pid).unwrap();
        p.intent = Intent::None; p.target = None;
    }
}
fn do_attack(g: &mut Game, pid: Pid, mid: Mid) {
    let (atk, str_) = { let p = g.players.get(&pid).unwrap(); (p.skills.attack, p.skills.strength) };
    let (mdef, mx, my) = match g.mobs.get(&mid) { Some(m) => (m.defence, m.x, m.y), _ => return };
    let dmg = roll_hit(atk, mdef, str_);
    let killed = {
        let m = g.mobs.get_mut(&mid).unwrap();
        m.hp_cur = (m.hp_cur - dmg).max(0);
        m.hp_cur == 0
    };
    g.events.push(json!({"k": if dmg == 0 { "miss_mob" } else { "hit_mob" }, "x": mx, "y": my, "dmg": dmg}));
    let p = g.players.get_mut(&pid).unwrap();
    p.log.push(if dmg == 0 { "You miss the goblin.".into() } else { format!("You hit the goblin for {}.", dmg) });
    p.skills.attack_xp += 8 + dmg * 4;
    p.skills.attack = level_from_xp(p.skills.attack_xp);
    p.skills.strength_xp += dmg * 4;
    p.skills.strength = level_from_xp(p.skills.strength_xp);
    p.skills.hp_xp += dmg;
    p.skills.hp = 10 + level_from_xp(p.skills.hp_xp) - 1;
    if killed {
        p.log.push("You kill the goblin!".into());
        add_inv(p, "coins", 5);
        p.intent = Intent::None; p.target = None;
        let m = g.mobs.get_mut(&mid).unwrap();
        m.respawn_at = Some(g.tick + 20 * TPS);
    }
}

fn process_mob(g: &mut Game, mid: Mid) {
    let (pos, respawning, home) = {
        let m = match g.mobs.get(&mid) { Some(m) => m, None => return };
        ((m.x, m.y), m.respawn_at, m.home)
    };
    if let Some(t) = respawning {
        if g.tick >= t {
            let m = g.mobs.get_mut(&mid).unwrap();
            m.x = home.0; m.y = home.1; m.hp_cur = m.hp_max; m.respawn_at = None;
        }
        return;
    }
    let target = g.players.values()
        .filter(|p| manhattan((p.x, p.y), pos) <= 6)
        .min_by_key(|p| manhattan((p.x, p.y), pos))
        .map(|p| (p.id, p.x, p.y));
    let Some((tpid, tx, ty)) = target else { return; };
    if chebyshev(pos, (tx, ty)) == 1 {
        let (matk, mstr) = { let m = g.mobs.get(&mid).unwrap(); (m.attack, m.strength) };
        let pdef = g.players.get(&tpid).unwrap().skills.defence;
        let dmg = roll_hit(matk, pdef, mstr);
        g.events.push(json!({"k": if dmg == 0 { "miss_player" } else { "hit_player" }, "x": tx, "y": ty, "dmg": dmg}));
        let p = g.players.get_mut(&tpid).unwrap();
        p.hp_cur = (p.hp_cur - dmg).max(0);
        p.log.push(if dmg == 0 { "The goblin misses you.".into() } else { format!("The goblin hits you for {}.", dmg) });
        p.skills.defence_xp += 4;
        p.skills.defence = level_from_xp(p.skills.defence_xp);
        if matches!(p.intent, Intent::None) && p.hp_cur > 0 {
            p.intent = Intent::Attack { mid };
            p.target = Some((pos.0, pos.1));
        }
        if p.hp_cur == 0 {
            p.log.push("You die! Respawning...".into());
            p.hp_cur = p.skills.hp;
            p.x = 20; p.y = 20;
            p.intent = Intent::None; p.target = None;
        }
    } else if let Some(mut path) = bfs(g, pos, GoalKind::Adjacent((tx, ty)), 0) {
        if let Some(step) = path.pop_front() {
            if g.walkable(step.0, step.1, 0) {
                let m = g.mobs.get_mut(&mid).unwrap();
                m.x = step.0; m.y = step.1;
            }
        }
    }
}

fn tick_world(g: &mut Game) {
    g.tick += 1;
    let now = g.tick;
    for i in 0..g.objects.len() {
        let new = match &g.objects[i] {
            Obj::Stump { tier, regrow } if *regrow <= now => Some(Obj::Tree { tier: *tier, hp: tree_def(*tier).hp }),
            Obj::DepletedRock { tier, regrow } if *regrow <= now => Some(Obj::Rock { tier: *tier, hp: rock_def(*tier).hp }),
            Obj::Bush { berries, regrow } if *berries < 3 && *regrow <= now =>
                Some(Obj::Bush { berries: berries + 1, regrow: now + 8 * TPS }),
            _ => None,
        };
        if let Some(o) = new { g.objects[i] = o; }
    }
    let pids: Vec<_> = g.players.keys().copied().collect();
    for pid in &pids { process_player(g, *pid); }
    let mids: Vec<_> = g.mobs.keys().copied().collect();
    for mid in &mids { process_mob(g, *mid); }
    for p in g.players.values_mut() {
        p.regen_ctr += 1;
        if p.regen_ctr as u64 >= 6 * TPS {
            p.regen_ctr = 0;
            if p.hp_cur < p.skills.hp { p.hp_cur += 1; }
        }
    }
}

fn build_map() -> (i32, i32, Vec<Tile>, Vec<Obj>) {
    let w: i32 = 40; let h: i32 = 40;
    let mut tiles = vec![Tile::Grass; (w * h) as usize];
    let mut objects = vec![Obj::None; (w * h) as usize];
    let idx = |x: i32, y: i32| (y * w + x) as usize;
    for x in 0..w { tiles[idx(x, 0)] = Tile::Water; tiles[idx(x, h - 1)] = Tile::Water; }
    for y in 0..h { tiles[idx(0, y)] = Tile::Water; tiles[idx(w - 1, y)] = Tile::Water; }
    for y in 2..11 { for x in 28..38 { tiles[idx(x, y)] = Tile::Stone; } }
    for y in 30..34 { for x in 5..16 { tiles[idx(x, y)] = Tile::Sand; } }
    for dy in -1..=1i32 { for dx in -1..=1i32 { tiles[idx(20 + dx, 18 + dy)] = Tile::Dirt; } }
    let trees = [
        (1, [(5,5),(6,5),(7,5),(5,6),(8,7),(15,8),(16,9),(14,12),(10,15),(8,20)]),
        (2, [(12,22),(25,15),(28,20),(15,25),(7,28),(22,30),(6,15),(16,21),(11,7),(18,12)]),
        (3, [(30,33),(31,32),(32,33),(30,31),(28,30),(25,32),(34,28),(35,30),(27,34),(33,35)]),
    ];
    for (tier, spots) in trees {
        for (x, y) in spots { objects[idx(x, y)] = Obj::Tree { tier, hp: tree_def(tier).hp }; }
    }
    let rocks = [
        (1, [(29,3),(30,3),(31,3),(29,5),(33,4),(35,7)]),
        (2, [(32,9),(30,8),(34,10),(36,9),(28,8),(31,6)]),
        (3, [(35,3),(36,4),(37,6),(34,7),(36,10),(29,10)]),
    ];
    for (tier, spots) in rocks {
        for (x, y) in spots { objects[idx(x, y)] = Obj::Rock { tier, hp: rock_def(tier).hp }; }
    }
    let bushes = [(22,18),(18,22),(24,20),(19,16),(11,11),(13,17),(21,21)];
    for (x, y) in bushes { objects[idx(x, y)] = Obj::Bush { berries: 3, regrow: 0 }; }
    let boulders = [(27,12),(33,15),(8,18),(17,30),(26,28)];
    for (x, y) in boulders { objects[idx(x, y)] = Obj::Boulder; }
    for x in 18..23 { tiles[idx(x, 19)] = Tile::Path; }
    for y in 19..25 { tiles[idx(20, y)] = Tile::Path; }
    objects[idx(20, 18)] = Obj::Trader;
    (w, h, tiles, objects)
}

fn shop_catalog() -> Vec<Value> {
    TOOL_DEFS.iter().map(|t| json!({
        "item": t.item,
        "name": t.name,
        "kind": t.kind,
        "tier": t.tier,
        "buy": t.buy,
    })).collect()
}

fn sell_catalog() -> Vec<Value> {
    let mut out: Vec<Value> = TREE_DEFS.iter().map(|r| json!({
        "item": r.item,
        "name": r.name,
        "tier": r.tier,
        "sell": r.sell,
        "xp": r.xp,
    })).collect();
    out.extend(ROCK_DEFS.iter().map(|r| json!({
        "item": r.item,
        "name": r.name,
        "tier": r.tier,
        "sell": r.sell,
        "xp": r.xp,
    })));
    out.push(json!({ "item": "berries", "name": "Berries", "tier": 1, "sell": 1, "xp": 0 }));
    out
}

fn build_state_msg(g: &Game, pid: Pid) -> String {
    let p = &g.players[&pid];
    let axe_tier = best_tool(p, "axe").map(|t| t.tier).unwrap_or(0);
    let pickaxe_tier = best_tool(p, "pickaxe").map(|t| t.tier).unwrap_or(0);
    let players: Vec<Value> = g.players.values().map(|q| json!({
        "id": q.id, "x": q.x, "y": q.y, "name": q.name, "hp": q.hp_cur, "hp_max": q.skills.hp
    })).collect();
    let mobs: Vec<Value> = g.mobs.values().filter(|m| m.respawn_at.is_none()).map(|m| json!({
        "id": m.id, "kind": m.kind, "x": m.x, "y": m.y, "hp": m.hp_cur, "hp_max": m.hp_max
    })).collect();
    json!({
        "t": "state", "tick": g.tick, "tick_ms": TICK_MS,
        "you": { "id": p.id, "x": p.x, "y": p.y, "hp": p.hp_cur, "skills": p.skills,
                 "inv": p.inv, "axe_tier": axe_tier, "pickaxe_tier": pickaxe_tier,
                 "intent": p.intent, "target": p.target },
        "players": players, "mobs": mobs, "objects": g.objects, "log": p.log,
        "shop": shop_catalog(), "sells": sell_catalog(),
        "events": g.events,
    }).to_string()
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<Mutex<Game>>>) -> impl IntoResponse {
    ws.on_upgrade(move |s| handle_socket(s, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<Mutex<Game>>) {
    let (mut sink, mut stream) = socket.split();
    let join = match stream.next().await {
        Some(Ok(Message::Text(t))) => t,
        _ => return,
    };
    let v: Value = match serde_json::from_str(&join) { Ok(v) => v, _ => return };
    if v.get("t").and_then(|x| x.as_str()) != Some("join") { return; }
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("anon").to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let pid = new_pid();
    {
        let mut g = state.lock().await;
        let p = Player::new(pid, name.clone(), 20, 20, tx.clone());
        let init = json!({ "t": "init", "w": g.w, "h": g.h, "tiles": g.tiles, "you": pid }).to_string();
        let _ = tx.send(init);
        g.players.insert(pid, p);
        println!("Player {} ({}) joined", pid, name);
    }
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg)).await.is_err() { break; }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(text) = msg {
            let v: Value = match serde_json::from_str(&text) { Ok(v) => v, _ => continue };
            let mut g = state.lock().await;
            match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                "click" => {
                    let x = v.get("x").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                    let y = v.get("y").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                    click(&mut g, pid, x, y);
                }
                "stop" => {
                    if let Some(p) = g.players.get_mut(&pid) {
                        p.intent = Intent::None; p.target = None;
                    }
                }
                "eat" => {
                    let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    eat(&mut g, pid, s);
                }
                "buy" => {
                    let item = v.get("item").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    buy(&mut g, pid, &item);
                }
                "sell" => {
                    let s = v.get("slot").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    sell(&mut g, pid, s);
                }
                _ => {}
            }
        }
    }
    {
        let mut g = state.lock().await;
        g.players.remove(&pid);
        println!("Player {} left", pid);
    }
    send_task.abort();
}

fn resolve_static_root() -> PathBuf {
    if let Ok(p) = std::env::var("TRADSCAPE_ROOT") {
        return PathBuf::from(p);
    }
    let has_client = |dir: &PathBuf| {
        dir.join("index.html").exists() || dir.join("tradscape.html").exists()
    };
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
        .fallback_service(
            ServeDir::new(static_dir).append_index_html_on_directories(true),
        )
        .with_state(state);
    let listener = TcpListener::bind("0.0.0.0:8081").await.unwrap();
    println!("Tradscape listening on http://localhost:8081");
    axum::serve(listener, app).await.unwrap();
}
