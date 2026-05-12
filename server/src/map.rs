use crate::defs::{rock_def, tree_def};
use crate::types::{Obj, Tile};

pub struct MobSpawn {
    pub kind: &'static str,
    pub x: i32,
    pub y: i32,
}

pub struct MapDef {
    pub w: i32,
    pub h: i32,
    pub tiles: Vec<Tile>,
    pub objects: Vec<Obj>,
    pub mobs: Vec<MobSpawn>,
    pub player_spawn: (i32, i32),
}

include!(concat!(env!("OUT_DIR"), "/generated_map.rs"));

pub fn build_map_pub() -> MapDef {
    build_map()
}

/// Ocean columns west of the original 74-wide map; legacy map X is shifted by this offset.
pub const MAP_WEST_PAD: i32 = 18;
