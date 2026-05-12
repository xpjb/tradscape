//! WebTransport session handling.
//!
//! Each session opens a single bidirectional control stream over which all
//! reliable messages flow (handshake, init, baseline, chat, log, deltas, commands).
//! Per-tick delta frames are also pushed via QUIC datagrams when they fit, so the
//! client can act on the freshest tick without head-of-line blocking; if the
//! datagram is too large or fails, the next-tick stream send carries the latest
//! state forward (deltas are idempotent against the acked baseline).
//!
//! Stream framing: u32 LE length prefix, then payload. The first byte of payload
//! is the msg_type from wire.rs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;
use wtransport::{Endpoint, Identity, ServerConfig};

use crate::sim::{save_player_without_lock, valid_uuid_or_new, Sim};
use crate::wire::{self, ClientCmd, ClientWireState};

/// One inbound message from a client session, routed to the sim task.
pub enum SessionEvent {
    Join {
        pid_tx: tokio::sync::oneshot::Sender<u16>,
        uuid: String,
        name: String,
        out: UnboundedSender<Vec<u8>>,
    },
    Cmd { pid: u16, cmd: ClientCmd },
    Ack { pid: u16, tick: u32 },
    Disconnect { pid: u16 },
}

pub struct CertSummary {
    pub hash_hex: String,
}

pub async fn run_server(
    bind_addr: &str,
    sim: Arc<Mutex<Sim>>,
    cert_out: tokio::sync::oneshot::Sender<CertSummary>,
) -> anyhow::Result<()> {
    let identity = Identity::self_signed(["localhost", "127.0.0.1"])?;
    let chain = identity.certificate_chain();
    let cert = chain.as_slice().first().ok_or_else(|| anyhow::anyhow!("no cert"))?;
    let hash = cert.hash();
    let hash_hex: String = hash
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    let _ = cert_out.send(CertSummary { hash_hex: hash_hex.clone() });

    let bind: SocketAddr = bind_addr.parse()?;
    let config = ServerConfig::builder()
        .with_bind_address(bind)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .max_idle_timeout(Some(Duration::from_secs(30)))?
        .build();
    let endpoint = Endpoint::server(config)?;
    println!("WebTransport listening on https://{} (cert sha256={})", bind, hash_hex);

    let (session_tx, session_rx) = mpsc::unbounded_channel::<SessionEvent>();
    tokio::spawn(sim_task(sim.clone(), session_rx));

    loop {
        let incoming = endpoint.accept().await;
        let session_tx = session_tx.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(session_request) => match session_request.accept().await {
                    Ok(connection) => {
                        if let Err(err) = handle_session(connection, session_tx).await {
                            eprintln!("session error: {err}");
                        }
                    }
                    Err(err) => eprintln!("session accept: {err}"),
                },
                Err(err) => eprintln!("incoming session: {err}"),
            }
        });
    }
}

async fn handle_session(
    connection: wtransport::Connection,
    session_tx: UnboundedSender<SessionEvent>,
) -> anyhow::Result<()> {
    // Wait for client-initiated control stream
    let (mut send, mut recv) = connection.accept_bi().await?;

    // First message must be ClientJoin
    let first = read_frame(&mut recv).await?;
    if first.first().copied() != Some(wire::MSG_CLIENT_JOIN) {
        return Err(anyhow::anyhow!("first message must be ClientJoin"));
    }
    let mut r = wire::BufReader::new(&first[1..]);
    let uuid_raw = r.str_u8().unwrap_or_default();
    let uuid = valid_uuid_or_new(if uuid_raw.is_empty() { None } else { Some(&uuid_raw) });
    let name = r.str_u8().unwrap_or_else(|| "Adventurer".into());

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Ask sim to allocate a slot
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel::<u16>();
    session_tx.send(SessionEvent::Join {
        pid_tx,
        uuid: uuid.clone(),
        name,
        out: out_tx,
    })?;
    let pid = pid_rx.await?;

    // Forward server-bound messages to this socket
    let conn = connection.clone();
    let send_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if let Err(err) = write_frame(&mut send, &frame).await {
                eprintln!("send_frame: {err}");
                break;
            }
            // also push via datagram for low-latency tick delta paths
            if frame.first().copied() == Some(wire::MSG_SERVER_DELTA)
                || frame.first().copied() == Some(wire::MSG_SERVER_EVENTS)
            {
                let _ = conn.send_datagram(&frame[..]);
            }
        }
    });

    // Read loop for this session
    let session_tx_l = session_tx.clone();
    let read_task = tokio::spawn(async move {
        loop {
            let frame = match read_frame(&mut recv).await {
                Ok(f) => f,
                Err(_) => break,
            };
            let kind = match frame.first() { Some(k) => *k, None => continue };
            match kind {
                wire::MSG_CLIENT_CMD => {
                    if let Some(cmd) = wire::decode_command(&frame[1..]) {
                        let _ = session_tx_l.send(SessionEvent::Cmd { pid, cmd });
                    }
                }
                wire::MSG_CLIENT_ACK => {
                    if frame.len() >= 5 {
                        let tick = u32::from_le_bytes([frame[1], frame[2], frame[3], frame[4]]);
                        let _ = session_tx_l.send(SessionEvent::Ack { pid, tick });
                    }
                }
                _ => {}
            }
        }
        let _ = session_tx_l.send(SessionEvent::Disconnect { pid });
    });

    // Datagram read loop (acks may arrive as datagrams)
    let session_tx_d = session_tx.clone();
    let conn2 = connection.clone();
    tokio::spawn(async move {
        loop {
            let dgram = match conn2.receive_datagram().await {
                Ok(d) => d,
                Err(_) => break,
            };
            let bytes = dgram.payload();
            if bytes.first().copied() == Some(wire::MSG_CLIENT_ACK) && bytes.len() >= 5 {
                let tick = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
                let _ = session_tx_d.send(SessionEvent::Ack { pid, tick });
            }
        }
    });

    let _ = tokio::join!(send_task, read_task);
    let _ = session_tx.send(SessionEvent::Disconnect { pid });
    Ok(())
}

async fn read_frame(recv: &mut wtransport::RecvStream) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    read_exact(recv, &mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 4 * 1024 * 1024 {
        return Err(anyhow::anyhow!("frame too large: {}", len));
    }
    let mut buf = vec![0u8; len];
    read_exact(recv, &mut buf).await?;
    Ok(buf)
}

async fn read_exact(recv: &mut wtransport::RecvStream, buf: &mut [u8]) -> anyhow::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = recv.read(&mut buf[filled..]).await?;
        match n {
            Some(0) | None => return Err(anyhow::anyhow!("eof")),
            Some(k) => filled += k,
        }
    }
    Ok(())
}

async fn write_frame(send: &mut wtransport::SendStream, payload: &[u8]) -> anyhow::Result<()> {
    let len = (payload.len() as u32).to_le_bytes();
    send.write_all(&len).await?;
    send.write_all(payload).await?;
    Ok(())
}

// ─── Sim task: owns the Sim, processes events + ticks ───────────────────────

struct ClientState {
    pid: u16,
    uuid: String,
    out: UnboundedSender<Vec<u8>>,
    wire_state: ClientWireState,
    last_ack: u32,
}

async fn sim_task(sim: Arc<Mutex<Sim>>, mut events: UnboundedReceiver<SessionEvent>) {
    use crate::types::TICK_MS;
    let mut clients: Vec<ClientState> = Vec::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            ev = events.recv() => {
                let Some(ev) = ev else { return };
                let mut g = sim.lock().await;
                match ev {
                    SessionEvent::Join { pid_tx, uuid, name, out } => {
                        let cleaned = crate::sim::clean_name(&name);
                        let pid = g.join_player(&uuid, &cleaned);
                        let _ = pid_tx.send(pid);
                        let init = wire::encode_init(&g, pid, &uuid);
                        let _ = out.send(init);
                        let _ = out.send(wire::encode_catalogs());
                        let mut wire_state = ClientWireState::default();
                        let baseline = wire::encode_baseline(&g, &mut wire_state);
                        let _ = out.send(baseline);
                        let init_tick = g.tick as u32;
                        let _ = out.send(wire::encode_youview(&g, pid));
                        clients.push(ClientState { pid, uuid, out, wire_state, last_ack: init_tick });
                        println!("Player pid={} joined", pid);
                    }
                    SessionEvent::Cmd { pid, cmd } => {
                        handle_cmd(&mut g, pid, cmd);
                    }
                    SessionEvent::Ack { pid, tick } => {
                        if let Some(c) = clients.iter_mut().find(|c| c.pid == pid) {
                            c.last_ack = tick;
                        }
                    }
                    SessionEvent::Disconnect { pid } => {
                        if let Some(idx) = clients.iter().position(|c| c.pid == pid) {
                            let save = g.snapshot_player_save(pid);
                            g.cancel_player_trade(pid, "Other player disconnected.");
                            g.players.despawn(pid);
                            clients.swap_remove(idx);
                            if let Some(rec) = save {
                                save_player_without_lock(&rec);
                            }
                            println!("Player pid={} left", pid);
                        }
                    }
                }
            }
            _ = tick_interval.tick() => {
                let mut g = sim.lock().await;
                g.tick_world();
                // For each connected client: encode delta, log lines, chat updates, events
                for c in clients.iter_mut() {
                    let delta = wire::encode_delta(&g, &mut c.wire_state, c.last_ack);
                    let _ = c.out.send(delta);
                    let _ = c.out.send(wire::encode_youview(&g, c.pid));
                    if let Some(ev) = wire::encode_events(g.tick, &g.events) {
                        let _ = c.out.send(ev);
                    }
                    let i = c.pid as usize;
                    if i < g.players.alive.len() && g.players.alive[i] {
                        // Take log + private chat (drain), forward to client
                        let log = std::mem::take(&mut g.players.log[i]);
                        if let Some(lf) = wire::encode_log_lines(&log) {
                            let _ = c.out.send(lf);
                        }
                        let pchat = std::mem::take(&mut g.players.private_chat[i]);
                        // also include new global chat since their last_ack tick? simpler: flush latest 5
                        let recent: Vec<crate::types::ChatMsg> = g.chat.iter().rev().take(5).cloned().collect::<Vec<_>>().into_iter().rev().collect();
                        if let Some(cf) = wire::encode_chat(&recent, &pchat) {
                            let _ = c.out.send(cf);
                        }
                    }
                }
                // periodic SQLite save (every 2s as before)
                if g.tick % (10 * crate::types::TPS) == 0 {
                    g.save_all();
                }
            }
        }
    }
}

fn handle_cmd(g: &mut Sim, pid: u16, cmd: ClientCmd) {
    match cmd {
        ClientCmd::Click { x, y } => g.click(pid, x, y),
        ClientCmd::Attack { mid } => g.cmd_attack(pid, mid as u16),
        ClientCmd::TradePlayer { pid: other } => g.cmd_trade(pid, other as u16),
        ClientCmd::Stop => g.stop(pid),
        ClientCmd::Eat { slot } => g.eat(pid, slot as usize),
        ClientCmd::Drop { slot } => g.drop_one(pid, slot as usize),
        ClientCmd::Equip { slot } => g.equip_from_inv(pid, slot as usize),
        ClientCmd::Unequip { slot } => g.unequip_slot(pid, &slot),
        ClientCmd::Buy { item } => g.buy(pid, &item),
        ClientCmd::Sell { slot } => g.sell(pid, slot as usize),
        ClientCmd::CloseTrade => g.close_ui(pid, "trade"),
        ClientCmd::CloseForge => g.close_ui(pid, "forge"),
        ClientCmd::CloseAngler => g.close_ui(pid, "angler"),
        ClientCmd::AnglerBuy { item } => g.angler_buy(pid, &item),
        ClientCmd::Forge { item } => g.forge(pid, &item),
        ClientCmd::ClosePlayerTrade => g.close_player_trade(pid),
        ClientCmd::TradeOfferSlot { slot } => g.trade_offer_slot(pid, slot as usize),
        ClientCmd::TradeAccept => g.trade_accept(pid),
        ClientCmd::TradeConfirm => g.trade_confirm(pid),
        ClientCmd::AngelConfirm => g.angel_sacrifice(pid),
        ClientCmd::AngelDecline => g.angel_decline(pid),
        ClientCmd::Chat { text } => { g.add_chat(pid, &text); }
    }
}
