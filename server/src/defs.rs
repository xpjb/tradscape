//! Item, mob, resource, tool, armor, sword, fish definitions.
//! Pulled verbatim from the original main.rs.

use crate::types::Equipment;

pub const XP_CURVE_BASE: f64 = 50.0;
pub const XP_CURVE_MULT: f64 = 1.17;
pub const XP_MAX_LEVEL: i32 = 99;

pub fn xp_threshold_for_level(level: i32) -> i64 {
    if level <= 1 {
        return 0;
    }
    let steps = (level - 1) as f64;
    let numer = XP_CURVE_MULT.powf(steps) - 1.0;
    let denom = XP_CURVE_MULT - 1.0;
    (XP_CURVE_BASE * numer / denom).floor() as i64
}

pub fn level_from_xp(xp: i32) -> i32 {
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

pub fn xp_with_bonus(base: i32, angel_points: i32) -> i32 {
    ((base as f64) * (1.0 + angel_points as f64 * 0.01)).round() as i32
}

#[derive(Clone, Copy)]
pub struct MobDef {
    pub kind: &'static str,
    pub name: &'static str,
    pub tier: i32,
    pub hp: i32,
    pub attack: i32,
    pub strength: i32,
    pub defence: i32,
    pub aggro: i32,
    pub coin: i32,
}

pub const MOB_DEFS: [MobDef; 4] = [
    MobDef { kind: "goblin",      name: "goblin",      tier: 1, hp: 24,  attack: 8,  strength: 9,   defence: 4,  aggro: 6, coin: 5   },
    MobDef { kind: "club_goblin", name: "club goblin", tier: 2, hp: 60,  attack: 18, strength: 22,  defence: 12, aggro: 6, coin: 25  },
    MobDef { kind: "ninja",       name: "ninja",       tier: 3, hp: 140, attack: 45, strength: 50,  defence: 35, aggro: 8, coin: 90  },
    MobDef { kind: "dragon",      name: "dragon",      tier: 4, hp: 350, attack: 90, strength: 100, defence: 80, aggro: 6, coin: 225 },
];

pub fn mob_def(kind: &str) -> MobDef {
    MOB_DEFS.iter().find(|m| m.kind == kind).copied().unwrap_or(MOB_DEFS[0])
}

pub fn mob_kind_id(kind: &str) -> u8 {
    MOB_DEFS.iter().position(|m| m.kind == kind).unwrap_or(0) as u8
}

pub fn mob_kind_by_id(id: u8) -> &'static str {
    MOB_DEFS.get(id as usize).map(|m| m.kind).unwrap_or("goblin")
}

#[derive(Clone, Copy)]
pub struct ResourceDef {
    pub tier: i32,
    pub name: &'static str,
    pub item: &'static str,
    pub hp: i32,
    pub xp: i32,
    pub sell: i32,
    pub req_tool_tier: i32,
    pub regrow_secs: u64,
}

#[derive(Clone, Copy)]
pub struct ToolDef {
    pub item: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub tier: i32,
    pub buy: i32,
    pub power: i32,
}

pub const TREE_DEFS: [ResourceDef; 4] = [
    ResourceDef { tier: 1, name: "Pine Tree",  item: "pine_logs",  hp: 4,  xp: 20,  sell: 5,    req_tool_tier: 1, regrow_secs: 25  },
    ResourceDef { tier: 2, name: "Oak Tree",   item: "oak_logs",   hp: 10, xp: 55,  sell: 50,   req_tool_tier: 2, regrow_secs: 45  },
    ResourceDef { tier: 3, name: "Yew Tree",   item: "yew_logs",   hp: 22, xp: 130, sell: 500,  req_tool_tier: 3, regrow_secs: 75  },
    ResourceDef { tier: 4, name: "Magic Tree", item: "magic_logs", hp: 48, xp: 320, sell: 5000, req_tool_tier: 4, regrow_secs: 110 },
];

pub const ROCK_DEFS: [ResourceDef; 4] = [
    ResourceDef { tier: 1, name: "Copper Rock", item: "copper_ore", hp: 5,  xp: 25,  sell: 6,    req_tool_tier: 1, regrow_secs: 30  },
    ResourceDef { tier: 2, name: "Iron Rock",   item: "iron_ore",   hp: 12, xp: 65,  sell: 60,   req_tool_tier: 2, regrow_secs: 50  },
    ResourceDef { tier: 3, name: "Gold Rock",   item: "gold_ore",   hp: 26, xp: 150, sell: 600,  req_tool_tier: 3, regrow_secs: 80  },
    ResourceDef { tier: 4, name: "Cobalt Rock", item: "cobalt_ore", hp: 54, xp: 340, sell: 6000, req_tool_tier: 4, regrow_secs: 110 },
];

pub const TOOL_DEFS: [ToolDef; 8] = [
    ToolDef { item: "bronze_axe",     name: "Bronze Axe",     kind: "axe",     tier: 1, buy: 10,    power: 1 },
    ToolDef { item: "iron_axe",       name: "Iron Axe",       kind: "axe",     tier: 2, buy: 200,   power: 2 },
    ToolDef { item: "steel_axe",      name: "Steel Axe",      kind: "axe",     tier: 3, buy: 4000,  power: 4 },
    ToolDef { item: "cobalt_axe",     name: "Cobalt Axe",     kind: "axe",     tier: 4, buy: 30000, power: 8 },
    ToolDef { item: "bronze_pickaxe", name: "Bronze Pickaxe", kind: "pickaxe", tier: 1, buy: 10,    power: 1 },
    ToolDef { item: "iron_pickaxe",   name: "Iron Pickaxe",   kind: "pickaxe", tier: 2, buy: 200,   power: 2 },
    ToolDef { item: "steel_pickaxe",  name: "Steel Pickaxe",  kind: "pickaxe", tier: 3, buy: 4000,  power: 4 },
    ToolDef { item: "cobalt_pickaxe", name: "Cobalt Pickaxe", kind: "pickaxe", tier: 4, buy: 30000, power: 8 },
];

#[derive(Clone, Copy)]
pub struct FishDef {
    pub tier: i32,
    pub fish: &'static str,
    pub fish_name: &'static str,
    pub rod: &'static str,
    pub rod_name: &'static str,
    pub resource: &'static str,
    pub heal: i32,
    pub sell: i32,
    pub xp: i32,
}

pub const FISH_DEFS: [FishDef; 4] = [
    FishDef { tier: 0, fish: "yabby",      fish_name: "Yabby",      rod: "yabbypot", rod_name: "Yabby Pot",  resource: "pine_logs",  heal: 4,  sell: 4,   xp: 8   },
    FishDef { tier: 1, fish: "trout",      fish_name: "Trout",      rod: "oakrod",   rod_name: "Oak Rod",    resource: "oak_logs",   heal: 8,  sell: 16,  xp: 32  },
    FishDef { tier: 2, fish: "salmon",     fish_name: "Salmon",     rod: "yewrod",   rod_name: "Yew Rod",    resource: "yew_logs",   heal: 16, sell: 64,  xp: 128 },
    FishDef { tier: 3, fish: "anglerfish", fish_name: "Anglerfish", rod: "magicrod", rod_name: "Magic Rod",  resource: "magic_logs", heal: 32, sell: 256, xp: 512 },
];

pub fn fish_def_by_rod(rod: &str) -> Option<FishDef> {
    FISH_DEFS.iter().find(|f| f.rod == rod).copied()
}
pub fn fish_def_by_fish(fish: &str) -> Option<FishDef> {
    FISH_DEFS.iter().find(|f| f.fish == fish).copied()
}
pub fn fish_min_level(tier: i32) -> i32 {
    10 * tier + 1
}
pub fn fish_catch_chance(level: i32, tier: i32) -> f32 {
    let net = (level - 10 * tier).max(0) as f32;
    let denom = (1u32 << (tier as u32)) as f32;
    (net / denom / 100.0).min(1.0)
}
pub fn is_rod_item(item: &str) -> bool {
    FISH_DEFS.iter().any(|f| f.rod == item)
}

#[derive(Clone, Copy)]
pub struct ArmorDef {
    pub item: &'static str,
    pub name: &'static str,
    pub slot: &'static str,
    pub tier: i32,
    pub ore: &'static str,
    pub ore_qty: i32,
    pub defence: i32,
}

const fn ad(item: &'static str, name: &'static str, slot: &'static str,
            tier: i32, ore: &'static str, qty: i32) -> ArmorDef {
    ArmorDef { item, name, slot, tier, ore, ore_qty: qty,
               defence: qty * (1 << (tier - 1) as u32) }
}
pub const ARMOR_DEFS: [ArmorDef; 16] = [
    ad("copper_helm",    "Copper Helm",    "helmet",    1, "copper_ore", 5),
    ad("copper_plate",   "Copper Plate",   "chest",     1, "copper_ore", 10),
    ad("copper_greaves", "Copper Greaves", "legs",      1, "copper_ore", 7),
    ad("copper_shield",  "Copper Shield",  "left_hand", 1, "copper_ore", 6),
    ad("iron_helm",      "Iron Helm",      "helmet",    2, "iron_ore",   5),
    ad("iron_plate",     "Iron Plate",     "chest",     2, "iron_ore",   10),
    ad("iron_greaves",   "Iron Greaves",   "legs",      2, "iron_ore",   7),
    ad("iron_shield",    "Iron Shield",    "left_hand", 2, "iron_ore",   6),
    ad("gold_helm",      "Gold Helm",      "helmet",    3, "gold_ore",   5),
    ad("gold_plate",     "Gold Plate",     "chest",     3, "gold_ore",   10),
    ad("gold_greaves",   "Gold Greaves",   "legs",      3, "gold_ore",   7),
    ad("gold_shield",    "Gold Shield",    "left_hand", 3, "gold_ore",   6),
    ad("cobalt_helm",    "Cobalt Helm",    "helmet",    4, "cobalt_ore", 5),
    ad("cobalt_plate",   "Cobalt Plate",   "chest",     4, "cobalt_ore", 10),
    ad("cobalt_greaves", "Cobalt Greaves", "legs",      4, "cobalt_ore", 7),
    ad("cobalt_shield",  "Cobalt Shield",  "left_hand", 4, "cobalt_ore", 6),
];
pub fn armor_def(item: &str) -> Option<ArmorDef> {
    ARMOR_DEFS.iter().find(|a| a.item == item).copied()
}

#[derive(Clone, Copy)]
pub struct SwordDef {
    pub item: &'static str,
    pub name: &'static str,
    pub tier: i32,
    pub ore: &'static str,
    pub ore_qty: i32,
    pub damage: i32,
}

const fn sd(item: &'static str, name: &'static str, tier: i32, ore: &'static str, qty: i32) -> SwordDef {
    SwordDef { item, name, tier, ore, ore_qty: qty, damage: qty * (1 << (tier - 1) as u32) }
}
pub const SWORD_DEFS: [SwordDef; 4] = [
    sd("copper_sword", "Copper Sword", 1, "copper_ore", 10),
    sd("iron_sword",   "Iron Sword",   2, "iron_ore",   10),
    sd("gold_sword",   "Gold Sword",   3, "gold_ore",   10),
    sd("cobalt_sword", "Cobalt Sword", 4, "cobalt_ore", 10),
];
pub fn sword_def(item: &str) -> Option<SwordDef> {
    SWORD_DEFS.iter().find(|s| s.item == item).copied()
}

pub fn tree_def(tier: i32) -> ResourceDef {
    let i = (tier - 1).clamp(0, TREE_DEFS.len() as i32 - 1) as usize;
    TREE_DEFS[i]
}
pub fn rock_def(tier: i32) -> ResourceDef {
    let i = (tier - 1).clamp(0, ROCK_DEFS.len() as i32 - 1) as usize;
    ROCK_DEFS[i]
}

pub fn item_equip_slot(item: &str) -> Option<&'static str> {
    if let Some(a) = ARMOR_DEFS.iter().find(|a| a.item == item) {
        return Some(a.slot);
    }
    if item.ends_with("_axe") || item.ends_with("_pickaxe") || item.ends_with("_sword") || is_rod_item(item) {
        Some("right_hand")
    } else {
        None
    }
}

pub fn item_name(item: &str) -> &'static str {
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
        "yabby" => return "Yabby",
        "trout" => return "Trout",
        "anglerfish" => return "Anglerfish",
        "fishing_rod" => return "Fishing rod",
        _ => {}
    }
    if let Some(f) = FISH_DEFS.iter().find(|f| f.rod == item) { return f.rod_name; }
    if let Some(t) = TOOL_DEFS.iter().find(|t| t.item == item) { return t.name; }
    if let Some(a) = ARMOR_DEFS.iter().find(|a| a.item == item) { return a.name; }
    if let Some(s) = SWORD_DEFS.iter().find(|s| s.item == item) { return s.name; }
    "Item"
}

pub const ANGLER_RESOURCE_QTY: i32 = 10;

pub fn armor_value(item: &str) -> Option<i32> {
    let a = ARMOR_DEFS.iter().find(|a| a.item == item)?;
    let ore_unit = sell_value(a.ore)?;
    Some(ore_unit * a.ore_qty)
}
pub fn sword_value(item: &str) -> Option<i32> {
    let s = SWORD_DEFS.iter().find(|s| s.item == item)?;
    let ore_unit = sell_value(s.ore)?;
    Some(ore_unit * s.ore_qty)
}
pub fn sell_value(item: &str) -> Option<i32> {
    if item == "berries" { return Some(1); }
    if let Some(f) = fish_def_by_fish(item) { return Some(f.sell); }
    if let Some(v) = rod_value(item) { return Some(v); }
    TREE_DEFS.iter().chain(ROCK_DEFS.iter()).find(|r| r.item == item).map(|r| r.sell)
}
pub fn rod_value(item: &str) -> Option<i32> {
    let f = fish_def_by_rod(item)?;
    let unit = sell_value(f.resource)?;
    Some(unit * ANGLER_RESOURCE_QTY)
}
pub fn buy_price(item: &str) -> Option<i32> {
    TOOL_DEFS.iter().find(|t| t.item == item).map(|t| t.buy)
}
pub fn item_value(item: &str) -> Option<i32> {
    buy_price(item).or_else(|| armor_value(item)).or_else(|| sword_value(item)).or_else(|| sell_value(item))
}

pub fn tool_def(item: &str) -> Option<ToolDef> {
    TOOL_DEFS.iter().find(|t| t.item == item).copied()
}

pub fn armor_defence_bonus(equipment: &Equipment) -> i32 {
    let mut total = 0;
    for slot in crate::types::EQUIP_SLOT_NAMES {
        let it = equipment.get(slot);
        if !it.is_empty() {
            if let Some(a) = armor_def(it) { total += a.defence; }
        }
    }
    total
}

pub fn weapon_damage_bonus(equipment: &Equipment) -> i32 {
    sword_def(&equipment.right_hand).map(|s| s.damage).unwrap_or(0)
}
