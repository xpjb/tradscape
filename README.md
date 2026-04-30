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
- **Left-click tree / rock / berry bush / goblin / trader** — auto-walk adjacent and act. Traders open a trade window.
- **Right-click world** — stop current action.
- **Left-click berry slot** — eat (+3 HP).
- **Chat box** — send a global chat message. Commands: `/help`, `/nick name`.

You spawn with no coins and no tools. Pick berries, sell them to the trader, then buy an axe before chopping and a pickaxe before mining.
Player progress is saved in `tradscape.sqlite3` by browser-stored player UUID.

## Adding assets

Drop `.jpg` files into:

- `assets/tiles/` — `grass.jpg`, `dirt.jpg`, `sand.jpg`, `water.jpg`, `stone.jpg`
- `assets/entities/` — `tree.jpg`, `tree_stump.jpg`, `rock.jpg`, `rock_depleted.jpg`, `berry_bush.jpg`, `trader.jpg`, `goblin.jpg`, `player.jpg`
- `assets/items/` — `logs.jpg`, `ore.jpg`, `berries.jpg`, `coins.jpg`, `axe.jpg`, `pickaxe.jpg`

Until they're added, the client renders solid-color squares with a letter label so the game is fully playable without art.

## Protocol (JSON over WS)

Client → Server:
- `{t:"join", uuid, name}`
- `{t:"click", x, y}`
- `{t:"attack", mid}`
- `{t:"stop"}`
- `{t:"eat", slot}`
- `{t:"buy", item}`
- `{t:"sell", slot}`
- `{t:"close_trade"}`
- `{t:"chat", text}`

Server → Client:
- `{t:"init", w, h, tiles, you, uuid}` — once on connect
- `{t:"state", tick, you, players, mobs, objects, log, chat}` — every tick
