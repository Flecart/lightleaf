//! Real-time collaboration.
//!
//! Each open project gets a `Room`. Every text file that someone has open is backed by an
//! in-memory Yjs CRDT document (via `yrs`), so edits from many users merge without conflicts.
//! Dirty documents are flushed to the project's working tree by a background task and are
//! evicted from memory once nobody has had them open for a while — idle projects cost nothing.
//!
//! Binary frame layout (both directions): `[type u8][path_len u16 BE][path utf8][payload]`.

use crate::{
    auth::{User, user_from_session},
    error::{AppError, AppResult},
    state::{Role, SharedState},
    util::{is_text, join_safe, now},
};
use axum::{
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, mpsc};
use yrs::{
    Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, TextRef, Transact, Update,
    updates::{decoder::Decode, encoder::Encode},
};

const MSG_JOIN: u8 = 0;
const MSG_SYNC: u8 = 1;
const MSG_UPDATE: u8 = 2;
const MSG_LEAVE: u8 = 3;
const MSG_AWARENESS: u8 = 4;

const DOC_IDLE_EVICT: Duration = Duration::from_secs(60);
const MAX_DOC_BYTES: usize = 4 * 1024 * 1024;

const COLORS: [&str; 10] = [
    "#e6194b", "#3cb44b", "#4363d8", "#f58231", "#911eb4", "#46a0a0", "#f032e6", "#808000",
    "#9a6324", "#000075",
];

/// Messages queued per client before it is considered too slow and disconnected.
/// The browser reconnects and resyncs its documents, so nothing is lost.
const OUTBOX_CAPACITY: usize = 2048;

/// A client's bounded outgoing queue. Memory per client stays bounded no matter how slow its
/// network is, so one stalled tab cannot grow the server's memory without limit.
#[derive(Clone)]
struct Outbox {
    tx: mpsc::Sender<Message>,
    kick: Arc<Notify>,
}

impl Outbox {
    fn send(&self, msg: Message) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(msg) {
            self.kick.notify_one();
        }
    }
}

#[derive(Default)]
pub struct Hub {
    rooms: Mutex<HashMap<String, Arc<Room>>>,
    next_id: AtomicU64,
}

pub struct Room {
    pub id: String,
    root: PathBuf,
    inner: Mutex<RoomInner>,
}

#[derive(Default)]
struct RoomInner {
    clients: HashMap<u64, Client>,
    docs: HashMap<String, DocEntry>,
    /// Users who edited since the last git commit, used as commit authors.
    editors: HashMap<i64, (String, String)>,
}

struct Client {
    tx: Outbox,
    user: User,
    can_write: bool,
    color: &'static str,
    path: Option<String>,
}

struct DocEntry {
    doc: Doc,
    text: TextRef,
    epoch: u64,
    dirty: bool,
    subscribers: HashSet<u64>,
    idle_since: Instant,
}

impl DocEntry {
    fn new(content: &str) -> Self {
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let text = doc.get_or_insert_text("content");
        {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, content);
        }
        DocEntry {
            doc,
            text,
            epoch: rand::random::<u64>() >> 12, // keep it exactly representable as a JS number
            dirty: false,
            subscribers: HashSet::new(),
            idle_since: Instant::now(),
        }
    }

    fn content(&self) -> String {
        self.text.get_string(&self.doc.transact())
    }

    /// Replaces the document text with `new` using a minimal single-range edit and
    /// returns the resulting Yjs update (to broadcast), if anything changed.
    fn replace_content(&mut self, new: &str) -> Option<Vec<u8>> {
        let old = self.content();
        if old == new {
            return None;
        }
        let a: Vec<u16> = old.encode_utf16().collect();
        let b: Vec<u16> = new.encode_utf16().collect();
        let mut pre = 0;
        while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
            pre += 1;
        }
        let mut suf = 0;
        while suf < a.len() - pre
            && suf < b.len() - pre
            && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
        {
            suf += 1;
        }
        // Never split a UTF-16 surrogate pair.
        let low = |v: &[u16], i: usize| v.get(i).is_some_and(|c| (0xDC00..0xE000).contains(c));
        if pre > 0 && (low(&a, pre) || low(&b, pre)) {
            pre -= 1;
        }
        if suf > 0 && (low(&a, a.len() - suf) || low(&b, b.len() - suf)) {
            suf -= 1;
        }
        let suf = suf.min(a.len() - pre).min(b.len() - pre);
        let insert = String::from_utf16_lossy(&b[pre..b.len() - suf]);
        let before = self.doc.transact().state_vector();
        {
            let mut txn = self.doc.transact_mut();
            let del = (a.len() - pre - suf) as u32;
            if del > 0 {
                self.text.remove_range(&mut txn, pre as u32, del);
            }
            if !insert.is_empty() {
                self.text.insert(&mut txn, pre as u32, &insert);
            }
        }
        Some(self.doc.transact().encode_diff_v1(&before))
    }
}

fn frame(kind: u8, path: &str, payload: &[u8]) -> Message {
    let mut out = Vec::with_capacity(3 + path.len() + payload.len());
    out.push(kind);
    out.extend_from_slice(&(path.len() as u16).to_be_bytes());
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(payload);
    Message::Binary(out.into())
}

fn parse_frame(data: &[u8]) -> Option<(u8, &str, &[u8])> {
    if data.len() < 3 {
        return None;
    }
    let len = u16::from_be_bytes([data[1], data[2]]) as usize;
    let path = std::str::from_utf8(data.get(3..3 + len)?).ok()?;
    Some((data[0], path, &data[3 + len..]))
}

fn json_msg(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

impl Hub {
    pub fn room(&self, id: &str, root: PathBuf) -> Arc<Room> {
        let mut rooms = self.rooms.lock().unwrap();
        rooms
            .entry(id.to_string())
            .or_insert_with(|| {
                Arc::new(Room {
                    id: id.to_string(),
                    root,
                    inner: Mutex::new(RoomInner::default()),
                })
            })
            .clone()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Room>> {
        self.rooms.lock().unwrap().get(id).cloned()
    }

    fn all(&self) -> Vec<Arc<Room>> {
        self.rooms.lock().unwrap().values().cloned().collect()
    }

    pub fn close_room(&self, id: &str) {
        if let Some(room) = self.rooms.lock().unwrap().remove(id) {
            room.broadcast(json_msg(json!({ "type": "project-deleted" })), None);
            room.broadcast(Message::Close(None), None);
        }
    }
}

impl Room {
    fn broadcast(&self, msg: Message, except: Option<u64>) {
        let inner = self.inner.lock().unwrap();
        for (id, c) in &inner.clients {
            if Some(*id) != except {
                c.tx.send(msg.clone());
            }
        }
    }

    pub fn broadcast_json(&self, v: serde_json::Value) {
        self.broadcast(json_msg(v), None);
    }

    fn broadcast_presence(&self) {
        let users: Vec<_> = {
            let inner = self.inner.lock().unwrap();
            inner
                .clients
                .iter()
                .map(|(id, c)| json!({ "clientId": id, "userId": c.user.id, "name": c.user.name, "color": c.color, "path": c.path }))
                .collect()
        };
        self.broadcast_json(json!({ "type": "presence", "users": users }));
    }

    /// Takes the content of all dirty docs. Caller must hold the project lock and write them.
    fn take_dirty(&self) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .docs
            .iter_mut()
            .filter(|(_, d)| d.dirty)
            .map(|(p, d)| {
                d.dirty = false;
                (p.clone(), d.content())
            })
            .collect()
    }

    /// Writes dirty docs to disk. The caller must hold the project lock.
    pub async fn flush_locked(&self) -> AppResult<()> {
        for (path, content) in self.take_dirty() {
            let full = self.root.join(&path);
            if let Err(e) = tokio::fs::write(&full, content.as_bytes()).await {
                tracing::warn!("flush {}/{} failed: {e}", self.id, path);
                if let Some(d) = self.inner.lock().unwrap().docs.get_mut(&path) {
                    d.dirty = true;
                }
            }
        }
        Ok(())
    }

    /// Returns and clears the set of users who edited since the last commit.
    pub fn take_editors(&self) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap();
        inner.editors.drain().map(|(_, v)| v).collect()
    }

    /// Re-reads every loaded doc from disk (after a git push / restore / pull) and pushes
    /// minimal updates to connected editors. Caller must hold the project lock.
    pub async fn reload_from_disk(&self) {
        let paths: Vec<String> = self.inner.lock().unwrap().docs.keys().cloned().collect();
        for path in paths {
            let disk = tokio::fs::read(self.root.join(&path))
                .await
                .ok()
                .filter(|b| is_text(b));
            let mut inner = self.inner.lock().unwrap();
            let mut gone = false;
            let mut out = None;
            if let Some(entry) = inner.docs.get_mut(&path) {
                match disk {
                    None => gone = true,
                    Some(bytes) => {
                        let s = String::from_utf8_lossy(&bytes);
                        out = entry
                            .replace_content(&s)
                            .map(|u| (u, entry.subscribers.clone()));
                        entry.dirty = false;
                    }
                }
            }
            if gone {
                inner.docs.remove(&path);
            }
            if let Some((update, subs)) = out {
                let msg = frame(MSG_UPDATE, &path, &update);
                for id in subs {
                    if let Some(c) = inner.clients.get(&id) {
                        c.tx.send(msg.clone());
                    }
                }
            }
        }
        self.broadcast_json(json!({ "type": "tree" }));
    }

    /// Drops in-memory docs at or below `path` (after rename/delete). Caller flushes first.
    pub fn forget(&self, path: &str) {
        let prefix = format!("{path}/");
        let mut inner = self.inner.lock().unwrap();
        inner
            .docs
            .retain(|p, _| p != path && !p.starts_with(&prefix));
    }

    fn join(
        &self,
        cid: u64,
        path: &str,
        payload: &[u8],
        disk: Option<String>,
    ) -> Result<Message, String> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.docs.contains_key(path) {
            let content = disk.ok_or("not a text file")?;
            inner.docs.insert(path.to_string(), DocEntry::new(&content));
        }
        if let Some(c) = inner.clients.get_mut(&cid) {
            c.path = Some(path.to_string());
        }
        let entry = inner.docs.get_mut(path).unwrap();
        entry.subscribers.insert(cid);
        let client_epoch = payload
            .get(..8)
            .map(|b| u64::from_be_bytes(b.try_into().unwrap()))
            .unwrap_or(0);
        let sv = if client_epoch == entry.epoch {
            StateVector::decode_v1(&payload[8..]).unwrap_or_default()
        } else {
            StateVector::default()
        };
        let txn = entry.doc.transact();
        let diff = txn.encode_diff_v1(&sv);
        let server_sv = txn.state_vector().encode_v1();
        let mut out = Vec::with_capacity(12 + server_sv.len() + diff.len());
        out.extend_from_slice(&entry.epoch.to_be_bytes());
        out.extend_from_slice(&(server_sv.len() as u32).to_be_bytes());
        out.extend_from_slice(&server_sv);
        out.extend_from_slice(&diff);
        Ok(frame(MSG_SYNC, path, &out))
    }

    fn leave(&self, cid: u64, path: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(d) = inner.docs.get_mut(path) {
            d.subscribers.remove(&cid);
            if d.subscribers.is_empty() {
                d.idle_since = Instant::now();
            }
        }
        if let Some(c) = inner.clients.get_mut(&cid)
            && c.path.as_deref() == Some(path)
        {
            c.path = None;
        }
    }

    fn update(&self, cid: u64, path: &str, payload: &[u8], raw: Message) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let (can_write, user) = match inner.clients.get(&cid) {
            Some(c) => (c.can_write, c.user.clone()),
            None => return Err("unknown client".into()),
        };
        if !can_write {
            return Err("read-only access".into());
        }
        let entry = inner.docs.get_mut(path).ok_or("document is not open")?;
        let update = Update::decode_v1(payload).map_err(|e| format!("bad update: {e}"))?;
        entry
            .doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| format!("bad update: {e}"))?;
        if entry.text.len(&entry.doc.transact()) as usize > MAX_DOC_BYTES {
            tracing::warn!("doc {path} exceeds size limit");
        }
        entry.dirty = true;
        let subs: Vec<u64> = entry
            .subscribers
            .iter()
            .copied()
            .filter(|s| *s != cid)
            .collect();
        inner
            .editors
            .insert(user.id, (user.name.clone(), user.email.clone()));
        for id in subs {
            if let Some(c) = inner.clients.get(&id) {
                c.tx.send(raw.clone());
            }
        }
        Ok(())
    }

    fn relay_awareness(&self, cid: u64, path: &str, raw: Message) {
        let inner = self.inner.lock().unwrap();
        if let Some(d) = inner.docs.get(path) {
            for id in &d.subscribers {
                if *id != cid
                    && let Some(c) = inner.clients.get(id)
                {
                    c.tx.send(raw.clone());
                }
            }
        }
    }

    fn remove_client(&self, cid: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.clients.remove(&cid);
        for d in inner.docs.values_mut() {
            if d.subscribers.remove(&cid) && d.subscribers.is_empty() {
                d.idle_since = Instant::now();
            }
        }
    }

    /// Evicts idle docs; returns true when the room is completely empty.
    fn evict_idle(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.docs.retain(|_, d| {
            d.dirty || !d.subscribers.is_empty() || d.idle_since.elapsed() < DOC_IDLE_EVICT
        });
        inner.clients.is_empty() && inner.docs.is_empty()
    }
}

/// Flushes the project's dirty docs to disk (taking the project lock).
pub async fn flush_project(state: &SharedState, project_id: &str) -> AppResult<()> {
    if let Some(room) = state.hub.get(project_id) {
        let lock = state.project_lock(project_id);
        let _g = lock.lock().await;
        room.flush_locked().await?;
    }
    Ok(())
}

pub fn spawn_flusher(state: SharedState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            ticks += 1;
            if ticks.is_multiple_of(30) {
                state.prune_locks();
            }
            for room in state.hub.all() {
                {
                    let lock = state.project_lock(&room.id);
                    let _g = lock.lock().await;
                    let _ = room.flush_locked().await;
                }
                if room.evict_idle() {
                    let mut rooms = state.hub.rooms.lock().unwrap();
                    // Re-check under the hub lock so we never drop a room someone just joined.
                    if rooms.get(&room.id).is_some_and(|r| Arc::ptr_eq(r, &room))
                        && room.evict_idle()
                    {
                        rooms.remove(&room.id);
                    }
                }
            }
        }
    });
}

pub async fn flush_all(state: &SharedState) {
    for room in state.hub.all() {
        let lock = state.project_lock(&room.id);
        let _g = lock.lock().await;
        let _ = room.flush_locked().await;
    }
}

pub async fn ws_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    if !crate::security::same_origin(&headers) {
        return Err(AppError::forbidden());
    }
    let user = user_from_session(&state, &headers)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let role = state.role(&id, user.id).await?;
    let root = state.cfg.project_dir(&id);
    Ok(ws
        .max_message_size(8 * 1024 * 1024)
        .on_upgrade(move |socket| client_loop(state, id, root, user, role, socket)))
}

async fn client_loop(
    state: SharedState,
    pid: String,
    root: PathBuf,
    user: User,
    role: Role,
    socket: WebSocket,
) {
    let (mut sink, mut stream) = socket.split();
    let (sender, mut rx) = mpsc::channel::<Message>(OUTBOX_CAPACITY);
    let kick = Arc::new(Notify::new());
    let tx = Outbox {
        tx: sender,
        kick: kick.clone(),
    };
    let cid = state.hub.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let room = state.hub.room(&pid, root.clone());
    {
        let mut inner = room.inner.lock().unwrap();
        let color = COLORS[(user.id as usize) % COLORS.len()];
        inner.clients.insert(
            cid,
            Client {
                tx: tx.clone(),
                user: user.clone(),
                can_write: role.can_write(),
                color,
                path: None,
            },
        );
    }
    tx.send(json_msg(
        json!({ "type": "hello", "clientId": cid, "role": role.as_str() }),
    ));
    room.broadcast_presence();

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let close = matches!(msg, Message::Close(_));
            if sink.send(msg).await.is_err() || close {
                break;
            }
        }
    });

    loop {
        let msg = tokio::select! {
            m = stream.next() => match m {
                Some(Ok(m)) => m,
                _ => break,
            },
            _ = kick.notified() => {
                tracing::info!("disconnecting slow client {cid} of project {pid}");
                break;
            }
        };
        match msg {
            Message::Binary(data) => {
                let Some((kind, path, payload)) = parse_frame(&data) else {
                    continue;
                };
                let Ok((path, full)) = join_safe(&root, path) else {
                    continue;
                };
                match kind {
                    MSG_JOIN => {
                        let loaded = room.inner.lock().unwrap().docs.contains_key(&path);
                        let disk = if loaded {
                            None
                        } else {
                            match tokio::fs::read(&full).await {
                                Ok(b) if is_text(&b) => {
                                    Some(String::from_utf8(b).unwrap_or_default())
                                }
                                _ => None,
                            }
                        };
                        match room.join(cid, &path, payload, disk) {
                            Ok(m) => {
                                tx.send(m);
                            }
                            Err(e) => {
                                tx.send(json_msg(
                                    json!({ "type": "error", "path": path, "error": e }),
                                ));
                            }
                        }
                        room.broadcast_presence();
                    }
                    MSG_LEAVE => {
                        room.leave(cid, &path);
                        room.broadcast_presence();
                    }
                    MSG_UPDATE => {
                        if let Err(e) =
                            room.update(cid, &path, payload, Message::Binary(data.clone()))
                        {
                            tx.send(json_msg(
                                json!({ "type": "error", "path": path, "error": e }),
                            ));
                        }
                    }
                    MSG_AWARENESS => {
                        room.relay_awareness(cid, &path, Message::Binary(data.clone()))
                    }
                    _ => {}
                }
            }
            Message::Text(text) => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                if v["type"] == "chat" {
                    let body: String = v["body"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(4000)
                        .collect();
                    if body.is_empty() {
                        continue;
                    }
                    let (p, uid, b) = (pid.clone(), user.id, body.clone());
                    let ts = now();
                    let id = state
                        .db(move |c| {
                            c.execute(
                                "INSERT INTO chat (project_id, user_id, body, created_at) VALUES (?1, ?2, ?3, ?4)",
                                rusqlite::params![p, uid, b, ts],
                            )?;
                            Ok(c.last_insert_rowid())
                        })
                        .await
                        .unwrap_or(0);
                    room.broadcast_json(json!({ "type": "chat", "message": {
                        "id": id, "userId": user.id, "name": user.name, "body": body, "createdAt": ts } }));
                } else if v["type"] == "ping" {
                    tx.send(json_msg(json!({ "type": "pong" })));
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    room.remove_client(cid);
    room.broadcast_presence();
    drop(tx);
    writer.abort();
}
