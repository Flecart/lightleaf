// Real-time collaboration client. One WebSocket per project; each open file is a Y.Doc.
// Frame layout: [type u8][path_len u16 BE][path utf8][payload]
import * as Y from 'yjs';
import { Awareness, applyAwarenessUpdate, encodeAwarenessUpdate, removeAwarenessStates } from 'y-protocols/awareness';

const MSG_JOIN = 0;
const MSG_SYNC = 1;
const MSG_UPDATE = 2;
const MSG_LEAVE = 3;
const MSG_AWARENESS = 4;
const REMOTE = Symbol('remote');

const enc = new TextEncoder();
const dec = new TextDecoder();

function frame(type: number, path: string, payload: Uint8Array): Uint8Array {
  const p = enc.encode(path);
  const out = new Uint8Array(3 + p.length + payload.length);
  out[0] = type;
  out[1] = p.length >> 8;
  out[2] = p.length & 0xff;
  out.set(p, 3);
  out.set(payload, 3 + p.length);
  return out;
}

export interface PresenceUser {
  clientId: number;
  userId: number;
  name: string;
  color: string;
  path: string | null;
}

export interface ChatMessage {
  id: number;
  userId: number;
  name: string;
  body: string;
  createdAt: number;
}

export class DocBinding {
  doc: Y.Doc;
  text: Y.Text;
  awareness: Awareness;
  epoch = 0;
  synced = false;
  private syncWaiters: (() => void)[] = [];
  onReset: (() => void) | null = null;
  onChange: (() => void) | null = null;

  constructor(public path: string, private sock: ProjectSocket) {
    this.doc = new Y.Doc();
    this.text = this.doc.getText('content');
    this.awareness = new Awareness(this.doc);
    this.attach();
  }

  private attach() {
    this.doc.on('update', (update: Uint8Array, origin: unknown) => {
      if (origin !== REMOTE) this.sock.send(frame(MSG_UPDATE, this.path, update));
      this.onChange?.();
    });
    this.awareness.on('update', ({ added, updated, removed }: any, origin: unknown) => {
      if (origin === REMOTE) return;
      const changed = added.concat(updated, removed);
      this.sock.send(frame(MSG_AWARENESS, this.path, encodeAwarenessUpdate(this.awareness, changed)));
    });
  }

  whenSynced(): Promise<void> {
    if (this.synced) return Promise.resolve();
    return new Promise((r) => this.syncWaiters.push(r));
  }

  join() {
    const sv = this.epoch ? Y.encodeStateVector(this.doc) : new Uint8Array();
    const payload = new Uint8Array(8 + sv.length);
    new DataView(payload.buffer).setBigUint64(0, BigInt(this.epoch));
    payload.set(sv, 8);
    this.sock.send(frame(MSG_JOIN, this.path, payload));
  }

  handleSync(payload: Uint8Array) {
    const dv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const epoch = Number(dv.getBigUint64(0));
    const svLen = dv.getUint32(8);
    const serverSv = payload.subarray(12, 12 + svLen);
    const diff = payload.subarray(12 + svLen);
    if (this.epoch && this.epoch !== epoch) {
      // The server reloaded the document from disk: start over with a fresh Y.Doc.
      const user = this.awareness.getLocalState()?.user;
      this.awareness.destroy();
      this.doc.destroy();
      this.doc = new Y.Doc();
      this.text = this.doc.getText('content');
      this.awareness = new Awareness(this.doc);
      if (user) this.awareness.setLocalStateField('user', user);
      this.attach();
      Y.applyUpdate(this.doc, diff, REMOTE);
      this.epoch = epoch;
      this.onReset?.();
    } else {
      Y.applyUpdate(this.doc, diff, REMOTE);
      if (this.epoch === epoch) {
        // Send edits the server missed while we were offline.
        const missing = Y.encodeStateAsUpdate(this.doc, serverSv);
        if (missing.length > 2) this.sock.send(frame(MSG_UPDATE, this.path, missing));
      }
      this.epoch = epoch;
    }
    this.synced = true;
    this.syncWaiters.splice(0).forEach((r) => r());
    // Announce our cursor to peers already in the document.
    const local = this.awareness.getLocalState();
    if (local) this.awareness.setLocalState(local);
  }

  handleUpdate(payload: Uint8Array) {
    Y.applyUpdate(this.doc, payload, REMOTE);
  }

  handleAwareness(payload: Uint8Array) {
    applyAwarenessUpdate(this.awareness, payload, REMOTE);
  }

  destroy() {
    removeAwarenessStates(this.awareness, [this.doc.clientID], 'local');
    this.awareness.destroy();
    this.doc.destroy();
  }
}

type Handler = (msg: any) => void;

export class ProjectSocket {
  private ws: WebSocket | null = null;
  private docs = new Map<string, DocBinding>();
  private handlers = new Set<Handler>();
  private retry = 0;
  private closed = false;
  private pingTimer: number | undefined;
  clientId = 0;
  role: 'owner' | 'editor' | 'viewer' = 'viewer';
  connected = false;
  user: { name: string; color: string } = { name: 'Anonymous', color: '#888' };

  constructor(private projectId: string) {
    this.connect();
  }

  on(h: Handler) {
    this.handlers.add(h);
    return () => this.handlers.delete(h);
  }

  private emit(msg: any) {
    this.handlers.forEach((h) => h(msg));
  }

  private connect() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const ws = new WebSocket(`${proto}://${location.host}/api/projects/${this.projectId}/ws`);
    ws.binaryType = 'arraybuffer';
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 0;
      this.connected = true;
      this.emit({ type: 'connection', connected: true });
      for (const d of this.docs.values()) d.join();
      this.pingTimer = window.setInterval(() => this.sendJson({ type: 'ping' }), 25000);
    };
    ws.onclose = () => {
      this.connected = false;
      clearInterval(this.pingTimer);
      this.emit({ type: 'connection', connected: false });
      if (this.closed) return;
      const delay = Math.min(10000, 500 * 2 ** this.retry++);
      setTimeout(() => !this.closed && this.connect(), delay);
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === 'string') {
        const msg = JSON.parse(ev.data);
        if (msg.type === 'hello') {
          this.clientId = msg.clientId;
          this.role = msg.role;
        }
        this.emit(msg);
        return;
      }
      const data = new Uint8Array(ev.data as ArrayBuffer);
      const len = (data[1]! << 8) | data[2]!;
      const path = dec.decode(data.subarray(3, 3 + len));
      const payload = data.subarray(3 + len);
      const doc = this.docs.get(path);
      if (!doc) return;
      if (data[0] === MSG_SYNC) doc.handleSync(payload);
      else if (data[0] === MSG_UPDATE) doc.handleUpdate(payload);
      else if (data[0] === MSG_AWARENESS) doc.handleAwareness(payload);
    };
  }

  send(data: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(data);
  }

  sendJson(v: unknown) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(v));
  }

  open(path: string): DocBinding {
    let d = this.docs.get(path);
    if (d) return d;
    d = new DocBinding(path, this);
    d.awareness.setLocalStateField('user', { name: this.user.name, color: this.user.color, colorLight: this.user.color + '33' });
    this.docs.set(path, d);
    d.join();
    return d;
  }

  get(path: string) {
    return this.docs.get(path);
  }

  close(path: string) {
    const d = this.docs.get(path);
    if (!d) return;
    d.destroy();
    this.docs.delete(path);
    this.send(frame(MSG_LEAVE, path, new Uint8Array()));
  }

  /** Forget a doc without telling the server (e.g. it was renamed/deleted server-side). */
  drop(path: string) {
    const d = this.docs.get(path);
    if (!d) return;
    d.destroy();
    this.docs.delete(path);
  }

  destroy() {
    this.closed = true;
    for (const p of [...this.docs.keys()]) this.close(p);
    this.ws?.close();
  }
}
