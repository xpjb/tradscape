# Tradscape

RuneScape-flavored turn-based roguelike MMORPG. Rust authoritative server, vanilla JS / `<canvas>` client over WebSocket. 1Hz tick.

## Run

```sh
cd server
cargo run --release
```

Server listens on `http://localhost:8081` and serves the client at `/tradscape.html`. Open multiple tabs to test multiplayer.

## Controls

- **Left-click tile** — walk there.
- **Left-click tree / rock / berry bush / goblin / trader** — auto-walk adjacent and act.
- **Right-click world** — stop current action.
- **Left-click berry slot** — eat (+3 HP).
- **Right-click inventory slot (next to trader)** — sell.
- **Trade tab** — buy axe (10gp) or pickaxe (15gp) when adjacent to trader.

You spawn with 50 coins. Buy an axe before chopping, pickaxe before mining.

## Adding assets

Drop `.jpg` files into:

- `assets/tiles/` — `grass.jpg`, `dirt.jpg`, `sand.jpg`, `water.jpg`, `stone.jpg`
- `assets/entities/` — `tree.jpg`, `tree_stump.jpg`, `rock.jpg`, `rock_depleted.jpg`, `berry_bush.jpg`, `trader.jpg`, `goblin.jpg`, `player.jpg`
- `assets/items/` — `logs.jpg`, `ore.jpg`, `berries.jpg`, `coins.jpg`, `axe.jpg`, `pickaxe.jpg`

Until they're added, the client renders solid-color squares with a letter label so the game is fully playable without art.

## Protocol (JSON over WS)

Client → Server:
- `{t:"join", name}`
- `{t:"click", x, y}`
- `{t:"stop"}`
- `{t:"eat", slot}`
- `{t:"buy", item}` — `"axe"` or `"pickaxe"`
- `{t:"sell", slot}`

Server → Client:
- `{t:"init", w, h, tiles, you}` — once on connect
- `{t:"state", tick, you, players, mobs, objects, log}` — every tick
