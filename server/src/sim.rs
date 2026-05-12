//! Flat SoA simulation. Each entity type has its own pool of component arrays
//! (one Vec per replicated scalar) plus sidecar Vecs for non-uniform data.
//! Pool index is the entity handle; generations let stale references be detected.
//!
//! Game logic semantics match the legacy `Game`/`Player`/`Mob` code in
//! the pre-rewrite main.rs — only the storage shape changed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::defs::*;
use crate::map::{build_map_pub as build_map, MapDef, MAP_WEST_PAD};
use crate::types::*;

pub const DB_PATH: &str = "tradscape.sqlite3";

// ─────────────────────────────────────────────────────────────────────────────
//                                  Handles
// ─────────────────────────────────────────────────────────────────────────────

/// 32-bit packed handle: low 16 bits = pool index, high 16 bits = generation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Handle(pub u32);

impl Handle {
    pub const NONE: Handle = Handle(u32::MAX);
    pub fn new(idx: u16, gen: u16) -> Self {
        Self(((gen as u32) << 16) | idx as u32)
    }
    pub fn idx(self) -> usize {
        (self.0 & 0xFFFF) as usize
    }
    pub fn gen(self) -> u16 {
        (self.0 >> 16) as u16
    }
    pub fn is_none(self) -> bool {
        self.0 == u32::MAX
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//                                Player pool
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct PlayerPool {
    pub alive: Vec<bool>,
    pub gens: Vec<u16>,
    pub free: Vec<u16>,
    // SoA replicated scalars (these become bit-addressable components)
    pub xs: Vec<i16>,
    pub ys: Vec<i16>,
    pub hp_cur: Vec<i16>,
    pub hp_max: Vec<i16>,
    // sidecars
    pub uuid: Vec<String>,
    pub name: Vec<String>,
    pub skills: Vec<Skills>,
    pub inv: Vec<Vec<InvSlot>>,
    pub equipment: Vec<Equipment>,
    pub intent: Vec<Intent>,
    pub target: Vec<Option<(i32, i32)>>,
    pub trade: Vec<TradeState>,
    pub ui: Vec<UiState>,
    pub log: Vec<Vec<String>>,
    pub private_chat: Vec<Vec<ChatMsg>>,
    pub regen_ctr: Vec<u32>,
    pub last_ack: Vec<u32>,
    // spawn/despawn audit for snapshot encoding (cleared each tick)
    pub spawned: Vec<u16>,
    pub despawned: Vec<u16>,
}

impl PlayerPool {
    pub fn capacity(&self) -> usize {
        self.alive.len()
    }

    pub fn iter_live(&self) -> impl Iterator<Item = u16> + '_ {
        (0..self.alive.len()).filter_map(move |i| if self.alive[i] { Some(i as u16) } else { None })
    }

    pub fn spawn(&mut self, uuid: String, name: String, x: i32, y: i32) -> u16 {
        let idx = if let Some(i) = self.free.pop() {
            let i_us = i as usize;
            self.alive[i_us] = true;
            self.gens[i_us] = self.gens[i_us].wrapping_add(1);
            self.xs[i_us] = x as i16;
            self.ys[i_us] = y as i16;
            self.hp_cur[i_us] = 10;
            self.hp_max[i_us] = 10;
            self.uuid[i_us] = uuid;
            self.name[i_us] = name;
            self.skills[i_us] = Skills::starter();
            self.inv[i_us] = vec![InvSlot::default(); INV_SIZE];
            self.equipment[i_us] = Equipment::default();
            self.intent[i_us] = Intent::None;
            self.target[i_us] = None;
            self.trade[i_us] = TradeState::default();
            self.ui[i_us] = UiState::default();
            self.log[i_us] = Vec::new();
            self.private_chat[i_us] = Vec::new();
            self.regen_ctr[i_us] = 0;
            self.last_ack[i_us] = 0;
            i
        } else {
            let i = self.alive.len() as u16;
            self.alive.push(true);
            self.gens.push(0);
            self.xs.push(x as i16);
            self.ys.push(y as i16);
            self.hp_cur.push(10);
            self.hp_max.push(10);
            self.uuid.push(uuid);
            self.name.push(name);
            self.skills.push(Skills::starter());
            self.inv.push(vec![InvSlot::default(); INV_SIZE]);
            self.equipment.push(Equipment::default());
            self.intent.push(Intent::None);
            self.target.push(None);
            self.trade.push(TradeState::default());
            self.ui.push(UiState::default());
            self.log.push(Vec::new());
            self.private_chat.push(Vec::new());
            self.regen_ctr.push(0);
            self.last_ack.push(0);
            i
        };
        self.spawned.push(idx);
        idx
    }

    pub fn despawn(&mut self, idx: u16) {
        let i = idx as usize;
        if i >= self.alive.len() || !self.alive[i] {
            return;
        }
        self.alive[i] = false;
        self.free.push(idx);
        self.despawned.push(idx);
    }

    pub fn is_alive(&self, idx: u16) -> bool {
        (idx as usize) < self.alive.len() && self.alive[idx as usize]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//                                 Mob pool
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct MobPool {
    pub alive: Vec<bool>,
    pub gens: Vec<u16>,
    pub free: Vec<u16>,
    pub xs: Vec<i16>,
    pub ys: Vec<i16>,
    pub hp_cur: Vec<i16>,
    pub hp_max: Vec<i16>,
    pub kind_id: Vec<u8>,
    // sidecars
    pub attack: Vec<i32>,
    pub strength: Vec<i32>,
    pub defence: Vec<i32>,
    pub home: Vec<(i16, i16)>,
    pub respawn_at: Vec<u64>, // 0 = not respawning
    pub spawned: Vec<u16>,
    pub despawned: Vec<u16>,
}

impl MobPool {
    pub fn capacity(&self) -> usize {
        self.alive.len()
    }
    pub fn iter_live(&self) -> impl Iterator<Item = u16> + '_ {
        (0..self.alive.len()).filter_map(move |i| if self.alive[i] { Some(i as u16) } else { None })
    }
    pub fn spawn(&mut self, kind: &str, x: i32, y: i32) -> u16 {
        let d = mob_def(kind);
        let kid = mob_kind_id(kind);
        let idx = if let Some(i) = self.free.pop() {
            let u = i as usize;
            self.alive[u] = true;
            self.gens[u] = self.gens[u].wrapping_add(1);
            self.xs[u] = x as i16;
            self.ys[u] = y as i16;
            self.hp_cur[u] = d.hp as i16;
            self.hp_max[u] = d.hp as i16;
            self.kind_id[u] = kid;
            self.attack[u] = d.attack;
            self.strength[u] = d.strength;
            self.defence[u] = d.defence;
            self.home[u] = (x as i16, y as i16);
            self.respawn_at[u] = 0;
            i
        } else {
            let i = self.alive.len() as u16;
            self.alive.push(true);
            self.gens.push(0);
            self.xs.push(x as i16);
            self.ys.push(y as i16);
            self.hp_cur.push(d.hp as i16);
            self.hp_max.push(d.hp as i16);
            self.kind_id.push(kid);
            self.attack.push(d.attack);
            self.strength.push(d.strength);
            self.defence.push(d.defence);
            self.home.push((x as i16, y as i16));
            self.respawn_at.push(0);
            i
        };
        self.spawned.push(idx);
        idx
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//                            Ground item pool
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GroundPool {
    pub alive: Vec<bool>,
    pub gens: Vec<u16>,
    pub free: Vec<u16>,
    pub xs: Vec<i16>,
    pub ys: Vec<i16>,
    pub item: Vec<String>,
    pub qty: Vec<i32>,
    pub spawned: Vec<u16>,
    pub despawned: Vec<u16>,
}

impl GroundPool {
    pub fn iter_live(&self) -> impl Iterator<Item = u16> + '_ {
        (0..self.alive.len()).filter_map(move |i| if self.alive[i] { Some(i as u16) } else { None })
    }
    pub fn spawn(&mut self, x: i32, y: i32, item: &str, qty: i32) -> u16 {
        let idx = if let Some(i) = self.free.pop() {
            let u = i as usize;
            self.alive[u] = true;
            self.gens[u] = self.gens[u].wrapping_add(1);
            self.xs[u] = x as i16;
            self.ys[u] = y as i16;
            self.item[u] = item.to_string();
            self.qty[u] = qty;
            i
        } else {
            let i = self.alive.len() as u16;
            self.alive.push(true);
            self.gens.push(0);
            self.xs.push(x as i16);
            self.ys.push(y as i16);
            self.item.push(item.to_string());
            self.qty.push(qty);
            i
        };
        self.spawned.push(idx);
        idx
    }
    pub fn despawn(&mut self, idx: u16) {
        let i = idx as usize;
        if i >= self.alive.len() || !self.alive[i] {
            return;
        }
        self.alive[i] = false;
        self.free.push(idx);
        self.despawned.push(idx);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//                                Event record
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum EventRec {
    Chop { x: i16, y: i16 },
    Mine { x: i16, y: i16 },
    Pick { x: i16, y: i16 },
    Fish { x: i16, y: i16 },
    HitMob { x: i16, y: i16, dmg: i16 },
    MissMob { x: i16, y: i16 },
    HitPlayer { x: i16, y: i16, dmg: i16 },
    MissPlayer { x: i16, y: i16 },
}

// ─────────────────────────────────────────────────────────────────────────────
//                                  Sim
// ─────────────────────────────────────────────────────────────────────────────

pub struct Sim {
    pub w: i32,
    pub h: i32,
    pub tiles: Vec<Tile>,
    pub objects: Vec<Obj>,
    pub objects_dirty: Vec<u32>,
    pub player_spawn: (i32, i32),
    pub players: PlayerPool,
    pub mobs: MobPool,
    pub ground: GroundPool,
    pub tick: u64,
    pub chat_seq: u64,
    pub chat: VecDeque<ChatMsg>,
    pub events: Vec<EventRec>,
    pub daily_visitors_day: chrono::NaiveDate,
    pub daily_visitors: HashSet<String>,
    pub db: Connection,
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

pub fn manhattan(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}
pub fn chebyshev(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs())
}

#[derive(Clone, Copy)]
enum GoalKind {
    Step((i32, i32)),
    Adjacent((i32, i32)),
}

// ─────────────────────────────────────────────────────────────────────────────
//                              Construction
// ─────────────────────────────────────────────────────────────────────────────

impl Sim {
    pub fn new() -> Self {
        let MapDef {
            w,
            h,
            tiles,
            objects,
            mobs,
            player_spawn,
        } = build_map();
        let db = open_db();
        let today = chrono::Local::now().date_naive();
        let mut s = Self {
            w,
            h,
            objects_dirty: Vec::new(),
            tiles,
            objects,
            player_spawn,
            players: PlayerPool::default(),
            mobs: MobPool::default(),
            ground: GroundPool::default(),
            tick: 0,
            chat_seq: 0,
            chat: VecDeque::new(),
            events: Vec::new(),
            daily_visitors_day: today,
            daily_visitors: HashSet::new(),
            db,
        };
        for spawn in &mobs {
            s.mobs.spawn(spawn.kind, spawn.x, spawn.y);
        }
        s
    }

    pub fn idx(&self, x: i32, y: i32) -> usize {
        (y * self.w + x) as usize
    }
    pub fn in_b(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h
    }
    pub fn tile(&self, x: i32, y: i32) -> Tile {
        self.tiles[self.idx(x, y)]
    }
    pub fn obj(&self, x: i32, y: i32) -> &Obj {
        &self.objects[self.idx(x, y)]
    }
    fn set_obj(&mut self, x: i32, y: i32, o: Obj) {
        let i = self.idx(x, y);
        self.objects[i] = o;
        self.objects_dirty.push(i as u32);
    }

    fn drop_ground(&mut self, x: i32, y: i32, item: &str, qty: i32) {
        if qty <= 0 || item.is_empty() {
            return;
        }
        // merge into existing stack at same tile
        for i in 0..self.ground.alive.len() {
            if self.ground.alive[i]
                && self.ground.xs[i] == x as i16
                && self.ground.ys[i] == y as i16
                && self.ground.item[i] == item
            {
                self.ground.qty[i] += qty;
                return;
            }
        }
        self.ground.spawn(x, y, item, qty);
    }

    fn occupant_pid(&self, x: i32, y: i32) -> Option<u16> {
        for i in 0..self.players.alive.len() {
            if self.players.alive[i] && self.players.xs[i] == x as i16 && self.players.ys[i] == y as i16
            {
                return Some(i as u16);
            }
        }
        None
    }
    fn occupant_mid(&self, x: i32, y: i32) -> Option<u16> {
        for i in 0..self.mobs.alive.len() {
            if self.mobs.alive[i]
                && self.mobs.respawn_at[i] == 0
                && self.mobs.xs[i] == x as i16
                && self.mobs.ys[i] == y as i16
            {
                return Some(i as u16);
            }
        }
        None
    }
    fn walkable(&self, x: i32, y: i32, ignore_pid: i32) -> bool {
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
            if pid as i32 != ignore_pid {
                return false;
            }
        }
        if self.occupant_mid(x, y).is_some() {
            return false;
        }
        true
    }

    fn bfs(
        &self,
        from: (i32, i32),
        goal: GoalKind,
        ignore_pid: i32,
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
                (1, 0), (-1, 0), (0, 1), (0, -1),
                (1, 1), (1, -1), (-1, 1), (-1, -1),
            ] {
                let n = (cur.0 + dx, cur.1 + dy);
                if prev.contains_key(&n) {
                    continue;
                }
                if Some(n) == blocked {
                    continue;
                }
                if !self.walkable(n.0, n.1, ignore_pid) {
                    continue;
                }
                if dx != 0 && dy != 0 {
                    if !self.walkable(cur.0 + dx, cur.1, ignore_pid) {
                        continue;
                    }
                    if !self.walkable(cur.0, cur.1 + dy, ignore_pid) {
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
}

// ─────────────────────────────────────────────────────────────────────────────
//                           Inventory helpers
// ─────────────────────────────────────────────────────────────────────────────

fn add_inv_into(inv: &mut Vec<InvSlot>, item: &str, qty: i32) -> bool {
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
fn count_item_in(inv: &[InvSlot], item: &str) -> i32 {
    inv.iter().filter(|s| s.item == item).map(|s| s.qty).sum()
}
fn deduct_item_from(inv: &mut Vec<InvSlot>, item: &str, mut amt: i32) -> bool {
    if count_item_in(inv, item) < amt {
        return false;
    }
    for s in inv.iter_mut() {
        if s.item == item && s.qty > 0 {
            let take = amt.min(s.qty);
            s.qty -= take;
            amt -= take;
            if s.qty == 0 {
                *s = InvSlot::default();
            }
            if amt == 0 {
                return true;
            }
        }
    }
    amt == 0
}
fn has_item_in(inv: &[InvSlot], item: &str) -> bool {
    inv.iter().any(|s| s.item == item && s.qty > 0)
}
fn coin_count(inv: &[InvSlot]) -> i32 {
    inv.iter().filter(|s| s.item == "coins").map(|s| s.qty).sum()
}
fn deduct_coins(inv: &mut Vec<InvSlot>, amt: i32) -> bool {
    if coin_count(inv) < amt {
        return false;
    }
    let mut left = amt;
    for s in inv.iter_mut() {
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
fn best_tool(inv: &[InvSlot], kind: &str) -> Option<ToolDef> {
    inv.iter()
        .filter(|s| s.qty > 0)
        .filter_map(|s| tool_def(&s.item))
        .filter(|t| t.kind == kind)
        .max_by_key(|t| t.tier)
}
fn inventory_gp_value(inv: &[InvSlot]) -> i64 {
    let mut sum = 0i64;
    for s in inv {
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

fn equipped_rod_def(equipment: &Equipment) -> Option<FishDef> {
    fish_def_by_rod(&equipment.right_hand)
}
fn best_usable_rod(skills: &Skills, inv: &[InvSlot], equipment: &Equipment) -> Option<FishDef> {
    FISH_DEFS
        .iter()
        .copied()
        .filter(|f| skills.fishing >= fish_min_level(f.tier))
        .filter(|f| has_item_in(inv, f.rod) || equipment.right_hand == f.rod)
        .max_by_key(|f| f.tier)
}
fn highest_owned_rod(inv: &[InvSlot], equipment: &Equipment) -> Option<FishDef> {
    FISH_DEFS
        .iter()
        .copied()
        .filter(|f| has_item_in(inv, f.rod) || equipment.right_hand == f.rod)
        .max_by_key(|f| f.tier)
}
fn player_any_rod_available(inv: &[InvSlot], equipment: &Equipment) -> bool {
    highest_owned_rod(inv, equipment).is_some()
}
fn choose_active_rod(
    skills: &Skills,
    inv: &[InvSlot],
    equipment: &Equipment,
) -> Result<FishDef, String> {
    if let Some(eq) = equipped_rod_def(equipment) {
        return Ok(eq);
    }
    if let Some(rod) = best_usable_rod(skills, inv, equipment) {
        return Ok(rod);
    }
    if let Some(owned) = highest_owned_rod(inv, equipment) {
        return Err(format!(
            "Your fishing level is too low for the {} (need {}).",
            owned.rod_name,
            fish_min_level(owned.tier),
        ));
    }
    Err("You need a fishing rod.".to_string())
}

fn gather_success(skill: i32, resource_tier: i32) -> bool {
    let target = 1 + (resource_tier - 1) * 8;
    let chance = (0.45 + (skill - target) as f32 * 0.035).clamp(0.18, 0.92);
    rand_f() < chance
}
fn log_level_up(log: &mut Vec<String>, skill: &str, old_level: i32, new_level: i32) {
    if new_level > old_level {
        log.push(format!("Level up! {} is now {}.", skill, new_level));
    }
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

// ─────────────────────────────────────────────────────────────────────────────
//                          Game logic on pools
// ─────────────────────────────────────────────────────────────────────────────

impl Sim {
    pub fn near_obj_matching(&self, pid: u16, pred: impl Fn(&Obj) -> bool) -> bool {
        let i = pid as usize;
        let (px, py) = (self.players.xs[i] as i32, self.players.ys[i] as i32);
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                let nx = px + dx;
                let ny = py + dy;
                if self.in_b(nx, ny) && pred(self.obj(nx, ny)) {
                    return true;
                }
            }
        }
        false
    }
    fn near_trader(&self, pid: u16) -> bool {
        self.near_obj_matching(pid, |o| matches!(o, Obj::Trader))
    }
    fn near_blacksmith(&self, pid: u16) -> bool {
        self.near_obj_matching(pid, |o| matches!(o, Obj::Blacksmith))
    }
    fn near_angler(&self, pid: u16) -> bool {
        self.near_obj_matching(pid, |o| matches!(o, Obj::Angler))
    }
    fn near_angel(&self, pid: u16) -> bool {
        self.near_obj_matching(pid, |o| matches!(o, Obj::Angel))
    }

    pub fn click(&mut self, pid: u16, x: i32, y: i32) {
        if !self.in_b(x, y) {
            return;
        }
        if let Some(mid) = self.occupant_mid(x, y) {
            self.cmd_attack(pid, mid);
            return;
        }
        if let Some(other_pid) = self.occupant_pid(x, y) {
            if other_pid != pid {
                self.cmd_trade(pid, other_pid);
                return;
            }
        }
        let clicked_obj = self.obj(x, y).clone();
        let mut intent = match &clicked_obj {
            Obj::Tree { .. } => Intent::Chop,
            Obj::Rock { .. } => Intent::Mine,
            Obj::Bush { berries, .. } if *berries > 0 => Intent::Pick,
            Obj::Trader | Obj::Angel | Obj::Blacksmith | Obj::Angler => Intent::Talk,
            _ => Intent::None,
        };
        if matches!(intent, Intent::None) && matches!(self.tile(x, y), Tile::Water) {
            let i = pid as usize;
            if player_any_rod_available(&self.players.inv[i], &self.players.equipment[i]) {
                intent = Intent::Fish;
            }
        }
        if matches!(intent, Intent::None)
            && self.ground.iter_live().any(|h| {
                let u = h as usize;
                self.ground.xs[u] == x as i16 && self.ground.ys[u] == y as i16
            })
        {
            intent = Intent::Pickup;
        }
        let walk_ok = self.walkable(x, y, pid as i32);
        self.cancel_player_trade(pid, "Trade cancelled.");
        if !matches!(intent, Intent::Talk) {
            let u = pid as usize;
            self.players.ui[u] = UiState::default();
        } else {
            let u = pid as usize;
            let ui = &mut self.players.ui[u];
            *ui = UiState::default();
            match clicked_obj {
                Obj::Trader => ui.trade_open = true,
                Obj::Angel => ui.angel_modal_open = true,
                Obj::Blacksmith => ui.forge_open = true,
                Obj::Angler => ui.angler_open = true,
                _ => {}
            }
            // talk intent will open via process_player; ui state is set above only when adjacent.
            // mirror original which set these via process_player on goal — but easier to do here.
        }
        let u = pid as usize;
        if matches!(intent, Intent::None) {
            self.players.target[u] = if walk_ok { Some((x, y)) } else { None };
            self.players.intent[u] = Intent::None;
        } else {
            self.players.target[u] = Some((x, y));
            self.players.intent[u] = intent;
        }
    }

    pub fn cmd_attack(&mut self, pid: u16, mid: u16) {
        let m = mid as usize;
        if m >= self.mobs.alive.len() || !self.mobs.alive[m] || self.mobs.respawn_at[m] != 0 {
            return;
        }
        let target = (self.mobs.xs[m] as i32, self.mobs.ys[m] as i32);
        self.cancel_player_trade(pid, "Trade cancelled.");
        let u = pid as usize;
        self.players.ui[u] = UiState::default();
        self.players.intent[u] = Intent::Attack { mid: mid as u32 };
        self.players.target[u] = Some(target);
    }

    pub fn cmd_trade(&mut self, pid: u16, other_pid: u16) {
        if pid == other_pid {
            return;
        }
        let o = other_pid as usize;
        if !self.players.is_alive(other_pid) {
            return;
        }
        let target = (self.players.xs[o] as i32, self.players.ys[o] as i32);
        self.cancel_player_trade(pid, "Trade cancelled.");
        let u = pid as usize;
        self.players.ui[u] = UiState::default();
        self.players.intent[u] = Intent::Trade { pid: other_pid as u32 };
        self.players.target[u] = Some(target);
    }

    fn reset_trade_fields(&mut self, pid: u16) {
        let t = &mut self.players.trade[pid as usize];
        t.partner = None;
        t.stage = TradeStage::Offer;
        t.offer.clear();
        t.accepted = false;
        t.confirmed = false;
    }

    pub fn cancel_player_trade(&mut self, pid: u16, other_msg: &str) {
        let partner = self.players.trade[pid as usize].partner;
        self.reset_trade_fields(pid);
        if let Some(other_pid) = partner {
            let o = other_pid as usize;
            if o < self.players.alive.len() && self.players.alive[o]
                && self.players.trade[o].partner == Some(pid as u32)
            {
                self.reset_trade_fields(other_pid as u16);
                self.players.log[o].push(other_msg.into());
            }
        }
    }

    fn begin_player_trade(&mut self, a: u16, b: u16) {
        if !self.players.is_alive(a) || !self.players.is_alive(b) {
            return;
        }
        self.cancel_player_trade(a, "Trade cancelled.");
        self.cancel_player_trade(b, "Trade cancelled.");
        let a_name = self.players.name[a as usize].clone();
        let b_name = self.players.name[b as usize].clone();
        for (this_id, partner_id, partner_name) in [(a, b, b_name.clone()), (b, a, a_name.clone())] {
            let i = this_id as usize;
            self.reset_trade_fields(this_id);
            let t = &mut self.players.trade[i];
            t.partner = Some(partner_id as u32);
            t.request_from = None;
            self.players.ui[i] = UiState::default();
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            self.players.log[i].push(format!("Trading with {}.", partner_name));
        }
    }

    fn request_player_trade(&mut self, pid: u16, other_pid: u16) {
        if pid == other_pid || !self.players.is_alive(pid) || !self.players.is_alive(other_pid) {
            return;
        }
        if self.players.trade[pid as usize].request_from == Some(other_pid as u32) {
            self.begin_player_trade(pid, other_pid);
            return;
        }
        let name = self.players.name[pid as usize].clone();
        let other_name = self.players.name[other_pid as usize].clone();
        self.players.trade[other_pid as usize].request_from = Some(pid as u32);
        self.players.log[pid as usize].push(format!("Sending trade offer to {}...", other_name));
        self.push_private_chat(other_pid, format!("{} wishes to trade with you.", name));
    }

    fn reset_trade_accepts(&mut self, pid: u16) {
        let partner = self.players.trade[pid as usize].partner;
        for id in [Some(pid as u32), partner].into_iter().flatten() {
            let i = id as usize;
            if i < self.players.alive.len() && self.players.alive[i] {
                let t = &mut self.players.trade[i];
                t.stage = TradeStage::Offer;
                t.accepted = false;
                t.confirmed = false;
            }
        }
    }

    pub fn trade_offer_slot(&mut self, pid: u16, slot: usize) {
        if slot >= INV_SIZE {
            return;
        }
        let i = pid as usize;
        if self.players.trade[i].partner.is_none() {
            return;
        }
        if self.players.inv[i].get(slot).map(|s| s.item.is_empty()).unwrap_or(true) {
            return;
        }
        let t = &mut self.players.trade[i];
        if t.offer.contains(&slot) {
            t.offer.retain(|s| *s != slot);
        } else {
            t.offer.push(slot);
        }
        self.reset_trade_accepts(pid);
    }

    pub fn trade_accept(&mut self, pid: u16) {
        let i = pid as usize;
        let Some(partner) = self.players.trade[i].partner else { return };
        let p = partner as usize;
        if self.players.trade.get(p).and_then(|t| t.partner) != Some(pid as u32) {
            self.cancel_player_trade(pid, "Trade cancelled.");
            return;
        }
        self.players.trade[i].accepted = true;
        let both = self.players.trade[i].accepted && self.players.trade[p].accepted;
        if both {
            for id in [i, p] {
                let t = &mut self.players.trade[id];
                t.stage = TradeStage::Confirm;
                t.confirmed = false;
            }
        }
    }

    pub fn trade_confirm(&mut self, pid: u16) {
        let i = pid as usize;
        let Some(partner) = self.players.trade[i].partner else { return };
        let p = partner as usize;
        if self.players.trade[i].stage != TradeStage::Confirm
            || self.players.trade[p].stage != TradeStage::Confirm
        {
            return;
        }
        self.players.trade[i].confirmed = true;
        let both = self.players.trade[i].confirmed && self.players.trade[p].confirmed;
        if both {
            self.complete_player_trade(pid, partner as u16);
        }
    }

    fn complete_player_trade(&mut self, a: u16, b: u16) {
        let ai = a as usize;
        let bi = b as usize;
        if self.players.trade[ai].partner != Some(b as u32)
            || self.players.trade[bi].partner != Some(a as u32)
        {
            return;
        }
        let a_slots = self.players.trade[ai].offer.clone();
        let b_slots = self.players.trade[bi].offer.clone();
        let Some(a_offer) = offer_items_from_slots(&self.players.inv[ai], &a_slots) else {
            self.cancel_player_trade(a, "Trade cancelled.");
            return;
        };
        let Some(b_offer) = offer_items_from_slots(&self.players.inv[bi], &b_slots) else {
            self.cancel_player_trade(b, "Trade cancelled.");
            return;
        };
        let a_items: Vec<InvSlot> = a_offer.iter().map(|(_, it)| it.clone()).collect();
        let b_items: Vec<InvSlot> = b_offer.iter().map(|(_, it)| it.clone()).collect();
        if !can_receive_trade_items(&self.players.inv[ai], &b_items, &a_slots)
            || !can_receive_trade_items(&self.players.inv[bi], &a_items, &b_slots)
        {
            self.cancel_player_trade(a, "Trade cancelled.");
            self.players.log[ai].push("Trade cancelled: not enough inventory space.".into());
            return;
        }
        for (slot, _) in &a_offer {
            self.players.inv[ai][*slot] = InvSlot::default();
        }
        for it in &b_items {
            add_inv_into(&mut self.players.inv[ai], &it.item, it.qty);
        }
        self.reset_trade_fields(a);
        self.players.log[ai].push("Accepted trade.".into());

        for (slot, _) in &b_offer {
            self.players.inv[bi][*slot] = InvSlot::default();
        }
        for it in &a_items {
            add_inv_into(&mut self.players.inv[bi], &it.item, it.qty);
        }
        self.reset_trade_fields(b);
        self.players.log[bi].push("Accepted trade.".into());
    }

    pub fn angel_decline(&mut self, pid: u16) {
        self.players.ui[pid as usize].angel_modal_open = false;
    }
    pub fn angel_sacrifice(&mut self, pid: u16) {
        if !self.near_angel(pid) {
            self.players.log[pid as usize].push("Stand next to the angel.".into());
            return;
        }
        let i = pid as usize;
        let gp = inventory_gp_value(&self.players.inv[i]);
        let gain = (gp / 1000) as i32;
        let new_points = self.players.skills[i].angel_points.saturating_add(gain);
        self.players.inv[i] = vec![InvSlot::default(); INV_SIZE];
        self.players.skills[i] = Skills::starter();
        self.players.skills[i].angel_points = new_points;
        self.players.hp_cur[i] = self.players.skills[i].hp as i16;
        self.players.intent[i] = Intent::None;
        self.players.target[i] = None;
        self.players.ui[i] = UiState::default();
        self.players.log[i].push(format!(
            "Your inventory is sacrificed ({} GP). You gain {} angel points. All levels reset. Angel points grant +1% XP each.",
            gp, gain
        ));
    }

    pub fn buy(&mut self, pid: u16, item: &str) {
        if !self.near_trader(pid) {
            self.players.log[pid as usize].push("Stand next to the trader.".into());
            return;
        }
        let Some(price) = buy_price(item) else { return };
        let i = pid as usize;
        if has_item_in(&self.players.inv[i], item) {
            self.players.log[i].push(format!("You already have a {}.", item_name(item)));
            return;
        }
        if !deduct_coins(&mut self.players.inv[i], price) {
            self.players.log[i].push("Not enough coins.".into());
            return;
        }
        if add_inv_into(&mut self.players.inv[i], item, 1) {
            self.players.log[i].push(format!("You buy a {}.", item_name(item)));
        } else {
            add_inv_into(&mut self.players.inv[i], "coins", price);
            self.players.log[i].push("Inventory full!".into());
        }
    }
    pub fn sell(&mut self, pid: u16, slot: usize) {
        if !self.near_trader(pid) {
            self.players.log[pid as usize].push("Stand next to the trader.".into());
            return;
        }
        let i = pid as usize;
        if slot >= INV_SIZE {
            return;
        }
        let item = self.players.inv[i][slot].item.clone();
        if item.is_empty() || item == "coins" {
            return;
        }
        let Some(unit) = item_value(&item) else {
            self.players.log[i].push("The trader does not buy that.".into());
            return;
        };
        let qty = self.players.inv[i][slot].qty;
        self.players.inv[i][slot] = InvSlot::default();
        add_inv_into(&mut self.players.inv[i], "coins", unit * qty);
        self.players.log[i].push(format!("Sold {}x{} for {}gp.", item, qty, unit * qty));
    }

    pub fn forge(&mut self, pid: u16, item: &str) {
        if !self.near_blacksmith(pid) {
            self.players.log[pid as usize].push("Stand next to the blacksmith.".into());
            return;
        }
        let Some((forged, name, ore, qty)) = forge_recipe(item) else { return };
        let i = pid as usize;
        if count_item_in(&self.players.inv[i], ore) < qty {
            self.players.log[i].push(format!(
                "You need {} {} to forge a {}.",
                qty,
                item_name(ore),
                name
            ));
            return;
        }
        let _ = deduct_item_from(&mut self.players.inv[i], ore, qty);
        if !add_inv_into(&mut self.players.inv[i], forged, 1) {
            for _ in 0..qty {
                add_inv_into(&mut self.players.inv[i], ore, 1);
            }
            self.players.log[i].push("Inventory full!".into());
            return;
        }
        self.players.log[i].push(format!(
            "The blacksmith forges a {} from {} {}.",
            name, qty, item_name(ore)
        ));
    }

    pub fn angler_buy(&mut self, pid: u16, rod: &str) {
        if !self.near_angler(pid) {
            self.players.log[pid as usize].push("Stand next to the angler.".into());
            return;
        }
        let Some(def) = fish_def_by_rod(rod) else { return };
        let i = pid as usize;
        if has_item_in(&self.players.inv[i], def.rod) || self.players.equipment[i].right_hand == def.rod {
            self.players.log[i].push(format!("You already have a {}.", def.rod_name));
            return;
        }
        if count_item_in(&self.players.inv[i], def.resource) < ANGLER_RESOURCE_QTY {
            self.players.log[i].push(format!(
                "The angler wants {} {} for the {}.",
                ANGLER_RESOURCE_QTY,
                item_name(def.resource),
                def.rod_name,
            ));
            return;
        }
        let _ = deduct_item_from(&mut self.players.inv[i], def.resource, ANGLER_RESOURCE_QTY);
        if !add_inv_into(&mut self.players.inv[i], def.rod, 1) {
            for _ in 0..ANGLER_RESOURCE_QTY {
                add_inv_into(&mut self.players.inv[i], def.resource, 1);
            }
            self.players.log[i].push("Inventory full!".into());
            return;
        }
        self.players.log[i].push(format!(
            "The angler hands you a {} for {} {}.",
            def.rod_name,
            ANGLER_RESOURCE_QTY,
            item_name(def.resource),
        ));
    }

    pub fn eat(&mut self, pid: u16, slot: usize) {
        let i = pid as usize;
        if slot >= INV_SIZE || self.players.inv[i][slot].qty <= 0 {
            return;
        }
        if self.players.hp_cur[i] as i32 >= self.players.skills[i].hp {
            self.players.log[i].push("You're already at full health.".into());
            return;
        }
        let item = self.players.inv[i][slot].item.clone();
        let (amt, msg) = if item == "berries" {
            (3, "You eat the berries. (+3 HP)".to_string())
        } else if let Some(f) = fish_def_by_fish(&item) {
            (
                f.heal,
                format!("You eat the {}. (+{} HP)", f.fish_name.to_lowercase(), f.heal),
            )
        } else {
            return;
        };
        self.players.inv[i][slot].qty -= 1;
        if self.players.inv[i][slot].qty == 0 {
            self.players.inv[i][slot] = InvSlot::default();
        }
        let max = self.players.skills[i].hp as i16;
        self.players.hp_cur[i] = (self.players.hp_cur[i] + amt as i16).min(max);
        self.players.log[i].push(msg);
    }

    pub fn drop_one(&mut self, pid: u16, slot: usize) {
        let i = pid as usize;
        if slot >= INV_SIZE {
            return;
        }
        let s = &mut self.players.inv[i][slot];
        if s.item.is_empty() || s.qty <= 0 {
            return;
        }
        let item = s.item.clone();
        s.qty -= 1;
        if s.qty == 0 {
            *s = InvSlot::default();
        }
        let (x, y) = (self.players.xs[i] as i32, self.players.ys[i] as i32);
        self.players.log[i].push(format!("You drop {}.", item_name(&item)));
        self.drop_ground(x, y, &item, 1);
    }

    pub fn equip_from_inv(&mut self, pid: u16, slot: usize) {
        let i = pid as usize;
        if slot >= INV_SIZE {
            return;
        }
        let item = self.players.inv[i][slot].item.clone();
        if item.is_empty() {
            return;
        }
        let Some(eq_slot) = item_equip_slot(&item) else { return };
        let qty = self.players.inv[i][slot].qty;
        let prev = self.players.equipment[i].get(eq_slot).to_string();
        if qty > 1 {
            self.players.inv[i][slot].qty -= 1;
        } else {
            self.players.inv[i][slot] = InvSlot::default();
        }
        self.players.equipment[i].set(eq_slot, item.clone());
        if !prev.is_empty() && !add_inv_into(&mut self.players.inv[i], &prev, 1) {
            let (x, y) = (self.players.xs[i] as i32, self.players.ys[i] as i32);
            self.drop_ground(x, y, &prev, 1);
            self.players.log[i].push(format!("{} falls to the ground.", item_name(&prev)));
        }
        self.players.log[i].push(format!("You equip {}.", item_name(&item)));
    }

    pub fn unequip_slot(&mut self, pid: u16, eq_slot: &str) {
        if !EQUIP_SLOT_NAMES.contains(&eq_slot) {
            return;
        }
        let i = pid as usize;
        let item = self.players.equipment[i].get(eq_slot).to_string();
        if item.is_empty() {
            return;
        }
        self.players.equipment[i].set(eq_slot, String::new());
        if !add_inv_into(&mut self.players.inv[i], &item, 1) {
            let (x, y) = (self.players.xs[i] as i32, self.players.ys[i] as i32);
            self.drop_ground(x, y, &item, 1);
            self.players.log[i].push(format!("{} falls to the ground.", item_name(&item)));
            return;
        }
        self.players.log[i].push(format!("You unequip {}.", item_name(&item)));
    }

    pub fn stop(&mut self, pid: u16) {
        self.cancel_player_trade(pid, "Trade cancelled.");
        let i = pid as usize;
        self.players.intent[i] = Intent::None;
        self.players.target[i] = None;
        let ui = &mut self.players.ui[i];
        ui.trade_open = false;
        ui.angel_modal_open = false;
        ui.angler_open = false;
    }

    pub fn close_ui(&mut self, pid: u16, which: &str) {
        let ui = &mut self.players.ui[pid as usize];
        match which {
            "trade" => ui.trade_open = false,
            "forge" => ui.forge_open = false,
            "angler" => ui.angler_open = false,
            _ => {}
        }
    }
    pub fn close_player_trade(&mut self, pid: u16) {
        self.cancel_player_trade(pid, "Trade declined.");
    }

    fn push_private_chat(&mut self, pid: u16, text: impl Into<String>) {
        self.chat_seq += 1;
        let msg = ChatMsg {
            id: self.chat_seq,
            tick: self.tick,
            pid: 0,
            name: "System".into(),
            text: text.into(),
        };
        let i = pid as usize;
        if i < self.players.alive.len() && self.players.alive[i] {
            self.players.private_chat[i].push(msg);
        }
    }

    pub fn add_chat(&mut self, pid: u16, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        let clean: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
        if clean.is_empty() {
            return false;
        }
        if clean == "/help" {
            self.push_private_chat(pid, "Commands: /help, /online, /nick name, /die");
            self.push_private_chat(pid, "Controls: left click to walk, chop, mine, fish (rod + water), attack, trade. Right click to stop.");
            return false;
        }
        if clean == "/online" {
            self.sync_daily_visitors_day();
            let n_online = self.players.iter_live().count();
            let n_unique_today = self.daily_visitors.len();
            let mut names: Vec<String> = self
                .players
                .iter_live()
                .map(|h| self.players.name[h as usize].clone())
                .collect();
            names.sort();
            let list = if names.is_empty() { "(none)".into() } else { names.join(", ") };
            self.push_private_chat(pid, format!("Online now: {n_online} | Unique players today: {n_unique_today}"));
            self.push_private_chat(pid, format!("Players: {list}"));
            return false;
        }
        if clean == "/die" {
            self.cancel_player_trade(pid, "Trade cancelled.");
            let spawn = self.player_spawn;
            let i = pid as usize;
            self.players.hp_cur[i] = self.players.skills[i].hp as i16;
            self.players.xs[i] = spawn.0 as i16;
            self.players.ys[i] = spawn.1 as i16;
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            self.players.ui[i] = UiState::default();
            self.players.log[i].push("You die! Respawning...".into());
            return true;
        }
        if let Some(rest) = clean.strip_prefix("/nick ") {
            let new_name = clean_name(rest);
            let i = pid as usize;
            let old_name = std::mem::replace(&mut self.players.name[i], new_name.clone());
            self.chat_seq += 1;
            self.chat.push_back(ChatMsg {
                id: self.chat_seq,
                tick: self.tick,
                pid: 0,
                name: "System".into(),
                text: format!("{} is now known as {}.", old_name, new_name),
            });
            while self.chat.len() > 50 {
                self.chat.pop_front();
            }
            return false;
        }
        if clean.starts_with('/') {
            self.push_private_chat(pid, "Unknown command. Try /help.");
            return false;
        }
        let name = self.players.name[pid as usize].clone();
        self.chat_seq += 1;
        self.chat.push_back(ChatMsg {
            id: self.chat_seq,
            tick: self.tick,
            pid: pid as u32,
            name,
            text: clean,
        });
        while self.chat.len() > 50 {
            self.chat.pop_front();
        }
        false
    }

    fn sync_daily_visitors_day(&mut self) {
        let today = chrono::Local::now().date_naive();
        if self.daily_visitors_day != today {
            self.daily_visitors_day = today;
            self.daily_visitors.clear();
        }
    }
    pub fn note_player_visit_today(&mut self, uuid: &str) {
        self.sync_daily_visitors_day();
        self.daily_visitors.insert(uuid.to_string());
    }

    // ─────── Per-tick processing ───────

    fn process_player(&mut self, pid: u16) {
        let i = pid as usize;
        if !self.players.alive[i] {
            return;
        }
        let pos = (self.players.xs[i] as i32, self.players.ys[i] as i32);
        let intent = self.players.intent[i];
        let mut target = self.players.target[i];

        target = match intent {
            Intent::Attack { mid } => {
                let m = mid as usize;
                if m < self.mobs.alive.len() && self.mobs.alive[m] && self.mobs.respawn_at[m] == 0 {
                    Some((self.mobs.xs[m] as i32, self.mobs.ys[m] as i32))
                } else {
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                    return;
                }
            }
            Intent::Trade { pid: other_pid } => {
                let o = other_pid as usize;
                if o < self.players.alive.len() && self.players.alive[o] {
                    Some((self.players.xs[o] as i32, self.players.ys[o] as i32))
                } else {
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                    self.players.log[i].push("They are no longer here.".into());
                    return;
                }
            }
            _ => target,
        };
        let Some(t) = target else { return };
        let needs_adj = !matches!(intent, Intent::None | Intent::Pickup);
        let at_goal = if needs_adj { chebyshev(pos, t) == 1 } else { pos == t };
        if at_goal {
            match intent {
                Intent::Chop => self.do_chop(pid, t),
                Intent::Mine => self.do_mine(pid, t),
                Intent::Pick => self.do_pick(pid, t),
                Intent::Fish => self.do_fish(pid, t),
                Intent::Pickup => self.do_pickup(pid, t),
                Intent::Attack { mid } => self.do_attack(pid, mid as u16),
                Intent::Trade { pid: other_pid } => {
                    self.request_player_trade(pid, other_pid as u16);
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                }
                Intent::Talk => {
                    let talk_obj = self.obj(t.0, t.1).clone();
                    let ui = &mut self.players.ui[i];
                    *ui = UiState::default();
                    match talk_obj {
                        Obj::Trader => ui.trade_open = true,
                        Obj::Angel => ui.angel_modal_open = true,
                        Obj::Blacksmith => ui.forge_open = true,
                        Obj::Angler => ui.angler_open = true,
                        _ => {}
                    }
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                }
                Intent::None => {}
            }
            return;
        }
        let goal_kind = if needs_adj { GoalKind::Adjacent(t) } else { GoalKind::Step(t) };
        if let Some(mut path) = self.bfs(pos, goal_kind, pid as i32) {
            if let Some(step) = path.pop_front() {
                if self.walkable(step.0, step.1, pid as i32) {
                    self.players.xs[i] = step.0 as i16;
                    self.players.ys[i] = step.1 as i16;
                }
            }
        } else {
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            self.players.log[i].push("Can't reach there.".into());
        }
    }

    fn do_chop(&mut self, pid: u16, t: (i32, i32)) {
        let i = pid as usize;
        let tool = best_tool(&self.players.inv[i], "axe");
        let level = self.players.skills[i].woodcutting;
        let Some(tool) = tool else {
            self.players.log[i].push("You need an axe.".into());
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            return;
        };
        let obj = self.obj(t.0, t.1).clone();
        if let Obj::Tree { tier, hp } = obj {
            let def = tree_def(tier);
            if tool.tier < def.req_tool_tier {
                self.players.log[i].push(format!("You need a tier {} axe for {}.", def.req_tool_tier, def.name));
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
                return;
            }
            self.events.push(EventRec::Chop { x: t.0 as i16, y: t.1 as i16 });
            if !gather_success(level, tier) {
                self.players.log[i].push(format!("You fail to cut the {}.", def.name));
                return;
            }
            let dmg = tool.power + level / 12;
            let new_hp = hp - dmg;
            if new_hp <= 0 {
                self.set_obj(t.0, t.1, Obj::Stump { tier, regrow: self.tick + def.regrow_secs * TPS });
                if add_inv_into(&mut self.players.inv[i], def.item, 1) {
                    self.players.log[i].push(format!("You get {}.", item_name(def.item)));
                } else {
                    self.players.log[i].push("Inventory full!".into());
                }
                let ap = self.players.skills[i].angel_points;
                self.players.skills[i].woodcutting_xp += xp_with_bonus(def.xp, ap);
                let new_level = level_from_xp(self.players.skills[i].woodcutting_xp);
                self.players.skills[i].woodcutting = new_level;
                log_level_up(&mut self.players.log[i], "Woodcutting", level, new_level);
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
            } else {
                self.set_obj(t.0, t.1, Obj::Tree { tier, hp: new_hp });
            }
        } else {
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
        }
    }

    fn do_mine(&mut self, pid: u16, t: (i32, i32)) {
        let i = pid as usize;
        let tool = best_tool(&self.players.inv[i], "pickaxe");
        let level = self.players.skills[i].mining;
        let Some(tool) = tool else {
            self.players.log[i].push("You need a pickaxe.".into());
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            return;
        };
        let obj = self.obj(t.0, t.1).clone();
        if let Obj::Rock { tier, hp } = obj {
            let def = rock_def(tier);
            if tool.tier < def.req_tool_tier {
                self.players.log[i].push(format!("You need a tier {} pickaxe for {}.", def.req_tool_tier, def.name));
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
                return;
            }
            self.events.push(EventRec::Mine { x: t.0 as i16, y: t.1 as i16 });
            if !gather_success(level, tier) {
                self.players.log[i].push(format!("You fail to mine the {}.", def.name));
                return;
            }
            let dmg = tool.power + level / 12;
            let new_hp = hp - dmg;
            if new_hp <= 0 {
                self.set_obj(t.0, t.1, Obj::DepletedRock { tier, regrow: self.tick + def.regrow_secs * TPS });
                if add_inv_into(&mut self.players.inv[i], def.item, 1) {
                    self.players.log[i].push(format!("You mine {}.", item_name(def.item)));
                } else {
                    self.players.log[i].push("Inventory full!".into());
                }
                let ap = self.players.skills[i].angel_points;
                self.players.skills[i].mining_xp += xp_with_bonus(def.xp, ap);
                let new_level = level_from_xp(self.players.skills[i].mining_xp);
                self.players.skills[i].mining = new_level;
                log_level_up(&mut self.players.log[i], "Mining", level, new_level);
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
            } else {
                self.set_obj(t.0, t.1, Obj::Rock { tier, hp: new_hp });
            }
        } else {
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
        }
    }

    fn do_fish(&mut self, pid: u16, t: (i32, i32)) {
        let i = pid as usize;
        let level = self.players.skills[i].fishing;
        let rod_choice = choose_active_rod(&self.players.skills[i], &self.players.inv[i], &self.players.equipment[i]);
        let rod = match rod_choice {
            Ok(r) => r,
            Err(msg) => {
                self.players.log[i].push(msg);
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
                return;
            }
        };
        if !matches!(self.tile(t.0, t.1), Tile::Water) {
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            return;
        }
        if level < fish_min_level(rod.tier) {
            self.players.log[i].push(format!(
                "Your fishing level is too low for the {} (need {}).",
                rod.rod_name,
                fish_min_level(rod.tier),
            ));
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            return;
        }
        self.events.push(EventRec::Fish { x: t.0 as i16, y: t.1 as i16 });
        let chance = fish_catch_chance(level, rod.tier);
        if rand_f() >= chance {
            self.players.log[i].push(format!("The {} slips away.", rod.fish_name.to_lowercase()));
            return;
        }
        if !add_inv_into(&mut self.players.inv[i], rod.fish, 1) {
            self.players.log[i].push("Inventory full!".into());
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            return;
        }
        self.players.log[i].push(format!("You catch a {}.", rod.fish_name.to_lowercase()));
        let ap = self.players.skills[i].angel_points;
        self.players.skills[i].fishing_xp += xp_with_bonus(rod.xp, ap);
        let old_fishing = self.players.skills[i].fishing;
        self.players.skills[i].fishing = level_from_xp(self.players.skills[i].fishing_xp);
        log_level_up(&mut self.players.log[i], "Fishing", old_fishing, self.players.skills[i].fishing);
        self.players.intent[i] = Intent::None;
        self.players.target[i] = None;
    }

    fn do_pick(&mut self, pid: u16, t: (i32, i32)) {
        let i = pid as usize;
        let obj = self.obj(t.0, t.1).clone();
        if let Obj::Bush { berries, .. } = obj {
            if berries > 0 {
                let new_berries = berries - 1;
                self.set_obj(t.0, t.1, Obj::Bush { berries: new_berries, regrow: self.tick + 8 * TPS });
                self.events.push(EventRec::Pick { x: t.0 as i16, y: t.1 as i16 });
                if !add_inv_into(&mut self.players.inv[i], "berries", 1) {
                    self.players.log[i].push("Inventory full!".into());
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                    return;
                }
                self.players.log[i].push("You pick a berry.".into());
                if new_berries == 0 {
                    self.players.intent[i] = Intent::None;
                    self.players.target[i] = None;
                }
            } else {
                self.players.intent[i] = Intent::None;
                self.players.target[i] = None;
            }
        } else {
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
        }
    }

    fn do_pickup(&mut self, pid: u16, t: (i32, i32)) {
        let i = pid as usize;
        // collect matching live ground items at this tile
        let taken: Vec<u16> = self
            .ground
            .iter_live()
            .filter(|h| {
                let u = *h as usize;
                self.ground.xs[u] == t.0 as i16 && self.ground.ys[u] == t.1 as i16
            })
            .collect();
        self.players.intent[i] = Intent::None;
        self.players.target[i] = None;
        if taken.is_empty() {
            return;
        }
        let mut full = false;
        for h in taken {
            let u = h as usize;
            let item = self.ground.item[u].clone();
            let qty = self.ground.qty[u];
            self.ground.despawn(h);
            let added = if full {
                false
            } else {
                add_inv_into(&mut self.players.inv[i], &item, qty)
            };
            if !added {
                full = true;
                // re-spawn at the same tile (drop_ground merges with existing stacks of same item)
                let (x, y) = (self.ground.xs[u] as i32, self.ground.ys[u] as i32);
                self.drop_ground(x, y, &item, qty);
                continue;
            }
            self.players.log[i].push(format!("You pick up {}.", item_name(&item)));
        }
        if full {
            self.players.log[i].push("Inventory full!".into());
        }
    }

    fn do_attack(&mut self, pid: u16, mid: u16) {
        let i = pid as usize;
        let m = mid as usize;
        if m >= self.mobs.alive.len() || !self.mobs.alive[m] {
            return;
        }
        let w = weapon_damage_bonus(&self.players.equipment[i]);
        let atk = self.players.skills[i].attack + w;
        let str_ = self.players.skills[i].strength + w;
        let mdef = self.mobs.defence[m];
        let mx = self.mobs.xs[m] as i32;
        let my = self.mobs.ys[m] as i32;
        let kind = mob_kind_by_id(self.mobs.kind_id[m]);
        let dmg = roll_hit(atk, mdef, str_);
        self.mobs.hp_cur[m] = (self.mobs.hp_cur[m] - dmg as i16).max(0);
        let killed = self.mobs.hp_cur[m] == 0;
        self.events.push(if dmg == 0 {
            EventRec::MissMob { x: mx as i16, y: my as i16 }
        } else {
            EventRec::HitMob { x: mx as i16, y: my as i16, dmg: dmg as i16 }
        });
        self.players.log[i].push(if dmg == 0 {
            format!("You miss the {}.", kind)
        } else {
            format!("You hit the {} for {}.", kind, dmg)
        });
        let ap = self.players.skills[i].angel_points;
        self.players.skills[i].attack_xp += xp_with_bonus(8 + dmg * 4, ap);
        let old_attack = self.players.skills[i].attack;
        self.players.skills[i].attack = level_from_xp(self.players.skills[i].attack_xp);
        self.players.skills[i].strength_xp += xp_with_bonus(dmg * 4, ap);
        let old_strength = self.players.skills[i].strength;
        self.players.skills[i].strength = level_from_xp(self.players.skills[i].strength_xp);
        log_level_up(&mut self.players.log[i], "Attack", old_attack, self.players.skills[i].attack);
        log_level_up(&mut self.players.log[i], "Strength", old_strength, self.players.skills[i].strength);
        if killed {
            self.players.log[i].push(format!("You kill the {}!", kind));
            self.players.intent[i] = Intent::None;
            self.players.target[i] = None;
            let def = mob_def(kind);
            let drops = roll_mob_drops(def.tier);
            self.mobs.respawn_at[m] = self.tick + 20 * TPS;
            self.drop_ground(mx, my, "coins", def.coin);
            for (item, qty) in drops {
                self.drop_ground(mx, my, item, qty);
                self.players.log[i].push(format!("The {} drops {}!", kind, item_name(item)));
            }
        }
    }

    fn process_mob(&mut self, mid: u16) {
        let m = mid as usize;
        if !self.mobs.alive[m] {
            return;
        }
        let pos = (self.mobs.xs[m] as i32, self.mobs.ys[m] as i32);
        let respawning = self.mobs.respawn_at[m];
        let home = self.mobs.home[m];
        let kind = mob_kind_by_id(self.mobs.kind_id[m]);
        if respawning != 0 {
            if self.tick >= respawning {
                self.mobs.xs[m] = home.0;
                self.mobs.ys[m] = home.1;
                self.mobs.hp_cur[m] = self.mobs.hp_max[m];
                self.mobs.respawn_at[m] = 0;
            }
            return;
        }
        let aggro = mob_def(kind).aggro;
        let mut closest: Option<(u16, i32, i32, i32)> = None;
        for h in self.players.iter_live() {
            let u = h as usize;
            let px = self.players.xs[u] as i32;
            let py = self.players.ys[u] as i32;
            let d = manhattan((px, py), pos);
            if d <= aggro && closest.map(|c| d < c.3).unwrap_or(true) {
                closest = Some((h, px, py, d));
            }
        }
        let Some((tpid, tx, ty, _)) = closest else { return };
        if chebyshev(pos, (tx, ty)) == 1 {
            let matk = self.mobs.attack[m];
            let mstr = self.mobs.strength[m];
            let u = tpid as usize;
            let pdef = self.players.skills[u].defence + armor_defence_bonus(&self.players.equipment[u]);
            let dmg = roll_hit(matk, pdef, mstr);
            self.events.push(if dmg == 0 {
                EventRec::MissPlayer { x: tx as i16, y: ty as i16 }
            } else {
                EventRec::HitPlayer { x: tx as i16, y: ty as i16, dmg: dmg as i16 }
            });
            self.players.hp_cur[u] = (self.players.hp_cur[u] - dmg as i16).max(0);
            self.players.log[u].push(if dmg == 0 {
                format!("The {} misses you.", kind)
            } else {
                format!("The {} hits you for {}.", kind, dmg)
            });
            let ap = self.players.skills[u].angel_points;
            if dmg == 0 {
                self.players.skills[u].defence_xp += xp_with_bonus(8, ap);
                let old_defence = self.players.skills[u].defence;
                self.players.skills[u].defence = level_from_xp(self.players.skills[u].defence_xp);
                log_level_up(&mut self.players.log[u], "Defence", old_defence, self.players.skills[u].defence);
            } else {
                self.players.skills[u].hp_xp += xp_with_bonus(dmg, ap);
                let old_hp = self.players.skills[u].hp;
                self.players.skills[u].hp = 10 + level_from_xp(self.players.skills[u].hp_xp) - 1;
                log_level_up(&mut self.players.log[u], "HP", old_hp, self.players.skills[u].hp);
            }
            if matches!(self.players.intent[u], Intent::None) && self.players.hp_cur[u] > 0 {
                self.players.intent[u] = Intent::Attack { mid: mid as u32 };
                self.players.target[u] = Some((pos.0, pos.1));
            }
            if self.players.hp_cur[u] == 0 {
                let spawn = self.player_spawn;
                self.players.log[u].push("You die! Respawning...".into());
                self.players.hp_cur[u] = self.players.skills[u].hp as i16;
                self.players.xs[u] = spawn.0 as i16;
                self.players.ys[u] = spawn.1 as i16;
                self.players.intent[u] = Intent::None;
                self.players.target[u] = None;
                self.players.ui[u] = UiState::default();
            }
        } else if let Some(mut path) = self.bfs(pos, GoalKind::Adjacent((tx, ty)), -1) {
            if let Some(step) = path.pop_front() {
                if self.walkable(step.0, step.1, -1) {
                    self.mobs.xs[m] = step.0 as i16;
                    self.mobs.ys[m] = step.1 as i16;
                }
            }
        }
    }

    pub fn tick_world(&mut self) {
        self.tick += 1;
        // clear per-tick audit
        self.players.spawned.clear();
        self.players.despawned.clear();
        self.mobs.spawned.clear();
        self.mobs.despawned.clear();
        self.ground.spawned.clear();
        self.ground.despawned.clear();
        self.objects_dirty.clear();
        self.events.clear();

        let now = self.tick;
        for i in 0..self.objects.len() {
            let new = match &self.objects[i] {
                Obj::Stump { tier, regrow } if *regrow <= now => Some(Obj::Tree {
                    tier: *tier,
                    hp: tree_def(*tier).hp,
                }),
                Obj::DepletedRock { tier, regrow } if *regrow <= now => Some(Obj::Rock {
                    tier: *tier,
                    hp: rock_def(*tier).hp,
                }),
                Obj::Bush { berries, regrow } if *berries < 3 && *regrow <= now => {
                    Some(Obj::Bush {
                        berries: berries + 1,
                        regrow: now + 8 * TPS,
                    })
                }
                _ => None,
            };
            if let Some(o) = new {
                self.objects[i] = o;
                self.objects_dirty.push(i as u32);
            }
        }
        let pids: Vec<u16> = self.players.iter_live().collect();
        for pid in pids {
            self.process_player(pid);
        }
        let mids: Vec<u16> = self.mobs.iter_live().collect();
        for mid in mids {
            self.process_mob(mid);
        }
        // hp regen
        for i in 0..self.players.alive.len() {
            if !self.players.alive[i] {
                continue;
            }
            self.players.regen_ctr[i] += 1;
            if self.players.regen_ctr[i] as u64 >= 6 * TPS {
                self.players.regen_ctr[i] = 0;
                let max = self.players.skills[i].hp as i16;
                if self.players.hp_cur[i] < max {
                    self.players.hp_cur[i] += 1;
                }
            }
        }
    }

    /// Player-derived computed fields (best tools, rod, etc.) that aren't pure components.
    pub fn player_derived(&self, pid: u16) -> PlayerDerived {
        let i = pid as usize;
        let axe_tier = best_tool(&self.players.inv[i], "axe").map(|t| t.tier).unwrap_or(0);
        let pickaxe_tier = best_tool(&self.players.inv[i], "pickaxe").map(|t| t.tier).unwrap_or(0);
        let rod_tier = highest_owned_rod(&self.players.inv[i], &self.players.equipment[i])
            .map(|f| f.tier + 1)
            .unwrap_or(0);
        PlayerDerived {
            axe_tier,
            pickaxe_tier,
            rod_tier,
            armor_defence: armor_defence_bonus(&self.players.equipment[i]),
            weapon_damage: weapon_damage_bonus(&self.players.equipment[i]),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PlayerDerived {
    pub axe_tier: i32,
    pub pickaxe_tier: i32,
    pub rod_tier: i32,
    pub armor_defence: i32,
    pub weapon_damage: i32,
}

fn forge_recipe(item: &str) -> Option<(&'static str, &'static str, &'static str, i32)> {
    if let Some(a) = armor_def(item) {
        return Some((a.item, a.name, a.ore, a.ore_qty));
    }
    if let Some(s) = sword_def(item) {
        return Some((s.item, s.name, s.ore, s.ore_qty));
    }
    None
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
    add_inv_into(inv, item, qty)
}

fn can_receive_trade_items(inv: &[InvSlot], incoming: &[InvSlot], outgoing_slots: &[usize]) -> bool {
    let Some(outgoing) = offer_items_from_slots(inv, outgoing_slots) else {
        return false;
    };
    let mut inv = inv.to_vec();
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

// ─────────────────────────────────────────────────────────────────────────────
//                              Persistence
// ─────────────────────────────────────────────────────────────────────────────

pub fn clean_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(20)
        .collect::<String>()
        .trim()
        .to_string();
    if clean.is_empty() { "Adventurer".into() } else { clean }
}
pub fn valid_uuid_or_new(raw: Option<&str>) -> String {
    raw.and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::new_v4)
        .to_string()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PlayerSave {
    pub uuid: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub hp_cur: i32,
    pub skills: Skills,
    pub inv: Vec<InvSlot>,
    pub equipment: Equipment,
}

pub fn open_db() -> Connection {
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
    let ver: i32 = db.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0);
    if ver < 1 {
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

fn migrate_legacy_fishing_rod(inv: &mut [InvSlot], equipment: &mut Equipment) {
    for s in inv.iter_mut() {
        if s.item == "fishing_rod" && s.qty > 0 {
            s.item = "oakrod".into();
        }
    }
    if equipment.right_hand == "fishing_rod" {
        equipment.right_hand = "oakrod".into();
    }
}

pub fn load_saved_player(db: &Connection, uuid: &str) -> Option<PlayerSave> {
    db.query_row(
        "SELECT name, x, y, hp_cur, skills_json, inv_json, equipment_json FROM players WHERE uuid = ?1",
        params![uuid],
        |row| {
            let skills_json: String = row.get(4)?;
            let inv_json: String = row.get(5)?;
            let equipment_json: String = row.get(6)?;
            let mut equipment: Equipment = serde_json::from_str(&equipment_json).unwrap_or_default();
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
            migrate_legacy_fishing_rod(&mut inv, &mut equipment);
            Ok(PlayerSave {
                uuid: uuid.to_string(),
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

pub fn save_player_record(db: &Connection, rec: &PlayerSave) {
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

pub fn save_player_without_lock(rec: &PlayerSave) {
    if let Ok(db) = Connection::open(DB_PATH) {
        save_player_record(&db, rec);
    }
}

impl Sim {
    pub fn snapshot_player_save(&self, pid: u16) -> Option<PlayerSave> {
        let i = pid as usize;
        if !self.players.alive[i] {
            return None;
        }
        Some(PlayerSave {
            uuid: self.players.uuid[i].clone(),
            name: self.players.name[i].clone(),
            x: self.players.xs[i] as i32,
            y: self.players.ys[i] as i32,
            hp_cur: self.players.hp_cur[i] as i32,
            skills: self.players.skills[i].clone(),
            inv: self.players.inv[i].clone(),
            equipment: self.players.equipment[i].clone(),
        })
    }

    pub fn save_all(&self) {
        for pid in self.players.iter_live() {
            if let Some(rec) = self.snapshot_player_save(pid) {
                save_player_record(&self.db, &rec);
            }
        }
    }

    pub fn join_player(
        &mut self,
        uuid: &str,
        requested_name: &str,
    ) -> u16 {
        let saved = load_saved_player(&self.db, uuid);
        let (name, x, y, hp_cur, skills, inv, equipment) = if let Some(s) = saved {
            let n = clean_name(&s.name);
            let hp_cur = s.hp_cur.clamp(1, s.skills.hp);
            (n, s.x, s.y, hp_cur, s.skills, s.inv, s.equipment)
        } else {
            (
                requested_name.to_string(),
                self.player_spawn.0,
                self.player_spawn.1,
                10,
                Skills::starter(),
                vec![InvSlot::default(); INV_SIZE],
                Equipment::default(),
            )
        };
        let idx = self.players.spawn(uuid.to_string(), name, x, y);
        let i = idx as usize;
        self.players.hp_cur[i] = hp_cur as i16;
        self.players.hp_max[i] = skills.hp as i16;
        self.players.skills[i] = skills;
        self.players.inv[i] = inv;
        self.players.equipment[i] = equipment;
        self.players.log[i].push("Welcome to Tradscape, /help for commands.".into());
        self.note_player_visit_today(uuid);
        idx
    }
}

pub fn resolve_static_root() -> PathBuf {
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
