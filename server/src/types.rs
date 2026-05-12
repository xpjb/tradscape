use serde::{Deserialize, Serialize};

pub const INV_SIZE: usize = 28;
pub const TICK_MS: u64 = 200;
pub const TPS: u64 = 1000 / TICK_MS;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Tile {
    #[default]
    Grass,
    Dirt,
    Sand,
    Water,
    Stone,
    Path,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Obj {
    #[default]
    None,
    Tree { tier: i32, hp: i32 },
    Stump { tier: i32, regrow: u64 },
    Rock { tier: i32, hp: i32 },
    DepletedRock { tier: i32, regrow: u64 },
    Bush { berries: i32, regrow: u64 },
    Boulder,
    Trader,
    Angel,
    #[allow(dead_code)]
    Blacksmith,
    #[allow(dead_code)]
    Angler,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Default, Debug)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Intent {
    #[default]
    None,
    Chop,
    Mine,
    Pick,
    Fish,
    Talk,
    Pickup,
    Attack { mid: u32 },
    Trade { pid: u32 },
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Skills {
    pub woodcutting: i32,
    pub mining: i32,
    pub fishing: i32,
    pub attack: i32,
    pub strength: i32,
    pub defence: i32,
    pub hp: i32,
    pub woodcutting_xp: i32,
    pub mining_xp: i32,
    pub fishing_xp: i32,
    pub attack_xp: i32,
    pub strength_xp: i32,
    pub defence_xp: i32,
    pub hp_xp: i32,
    pub angel_points: i32,
}

impl Skills {
    pub fn starter() -> Self {
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

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct InvSlot {
    pub item: String,
    pub qty: i32,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Equipment {
    pub helmet: String,
    pub chest: String,
    pub legs: String,
    pub left_hand: String,
    pub right_hand: String,
}

pub const EQUIP_SLOT_NAMES: [&str; 5] = ["helmet", "chest", "legs", "left_hand", "right_hand"];

impl Equipment {
    pub fn get(&self, slot: &str) -> &str {
        match slot {
            "helmet" => &self.helmet,
            "chest" => &self.chest,
            "legs" => &self.legs,
            "left_hand" => &self.left_hand,
            "right_hand" => &self.right_hand,
            _ => "",
        }
    }
    pub fn set(&mut self, slot: &str, item: String) {
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

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TradeStage {
    #[default]
    Offer,
    Confirm,
}

#[derive(Clone, Default, Debug)]
pub struct TradeState {
    pub request_from: Option<u32>,
    pub partner: Option<u32>,
    pub stage: TradeStage,
    pub offer: Vec<usize>,
    pub accepted: bool,
    pub confirmed: bool,
}

#[derive(Clone, Default, Debug)]
pub struct UiState {
    pub trade_open: bool,
    pub forge_open: bool,
    pub angler_open: bool,
    pub angel_modal_open: bool,
}

#[derive(Clone, Serialize, Debug)]
pub struct ChatMsg {
    pub id: u64,
    pub tick: u64,
    pub pid: u32,
    pub name: String,
    pub text: String,
}

#[derive(Clone, Serialize, Debug)]
pub struct GroundItemView {
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub item: String,
    pub qty: i32,
}
