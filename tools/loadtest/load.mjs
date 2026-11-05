// Load test for Lightleaf. See tools/loadtest/README.md.
//
// Users are grouped G per project. Each joins main.tex over the collaboration WebSocket, inserts a
// timestamped string every ~2 s and sends cursor (awareness) updates. Peers measure fan-out latency
// from the timestamps. In parallel, 50 HTTP loops call typical API endpoints.
//
//   BASE=http://127.0.0.1:18081 DURATION=60 WS=1 HTTP=1 node load.mjs tokens.json
import { createRequire } from 'node:module';
import fs from 'node:fs';
const require = createRequire(new URL('../../frontend/package.json', import.meta.url));
const Y = require('yjs');

const BASE = process.env.BASE ?? 'http://127.0.0.1:18081';
const tokens = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const G = 5, DURATION = Number(process.env.DURATION ?? 60) * 1000;
const enc = new TextEncoder(), dec = new TextDecoder();
const frame = (t, path, payload) => { const p = enc.encode(path); const o = new Uint8Array(3 + p.length + payload.length); o[0] = t; o[1] = p.length >> 8; o[2] = p.length & 255; o.set(p, 3); o.set(payload, 3 + p.length); return o; };
const hdr = (i) => ({ cookie: `ll_session=${tokens[i]}`, 'content-type': 'application/json' });
const pct = (a, p) => { if (!a.length) return NaN; const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };

// 1. Owners create projects and add members.
const projects = [];
for (let g = 0; g * G < tokens.length; g++) {
  const owner = g * G;
  const r = await fetch(`${BASE}/api/projects`, { method: 'POST', headers: hdr(owner), body: JSON.stringify({ name: `p${g}`, template: 'latex' }) });
  const { id } = await r.json();
  for (let k = 1; k < G && owner + k < tokens.length; k++)
    await fetch(`${BASE}/api/projects/${id}/members`, { method: 'POST', headers: hdr(owner), body: JSON.stringify({ email: `u${owner + k}@load.test`, role: 'editor' }) });
  projects.push(id);
}
console.log(`created ${projects.length} projects`);

// 2. WebSocket clients.
const lat = [], errors = { ws: 0, http: 0 }; let sent = 0, received = 0, open = 0, kicked = 0;
const clients = (process.env.WS === '0' ? [] : tokens).map((_, i) => new Promise((resolve) => {
  const pid = projects[Math.floor(i / G)];
  const ws = new WebSocket(`${BASE.replace('http', 'ws')}/api/projects/${pid}/ws`, { headers: { cookie: `ll_session=${tokens[i]}` } });
  ws.binaryType = 'arraybuffer';
  const doc = new Y.Doc(); const text = doc.getText('content');
  let timer;
  doc.on('update', (u, origin) => { if (origin !== 'remote' && ws.readyState === 1) { ws.send(frame(2, 'main.tex', u)); sent++; } });
  text.observe((ev) => {
    if (ev.transaction.origin !== 'remote') return;
    for (const d of ev.delta) if (typeof d.insert === 'string') {
      const m = d.insert.match(/<(\d+)>/); if (m) { lat.push(Date.now() - Number(m[1])); received++; }
    }
  });
  ws.onopen = () => { open++; const p = new Uint8Array(8); ws.send(frame(0, 'main.tex', p)); };
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') return;
    const data = new Uint8Array(ev.data); const len = (data[1] << 8) | data[2]; const payload = data.subarray(3 + len);
    if (data[0] === 1) {
      const dv = new DataView(payload.buffer, payload.byteOffset); const svLen = dv.getUint32(8);
      Y.applyUpdate(doc, payload.subarray(12 + svLen), 'remote');
      timer = setInterval(() => {
        text.insert(Math.floor(Math.random() * Math.max(1, text.length)), `<${Date.now()}>`);
        ws.send(frame(4, 'main.tex', enc.encode('{"cursor":' + Math.random() + '}')));
      }, 1500 + Math.random() * 1000);
    } else if (data[0] === 2) Y.applyUpdate(doc, payload, 'remote');
  };
  ws.onerror = () => { errors.ws++; };
  ws.onclose = () => { clearInterval(timer); open--; kicked++; resolve(); };
  setTimeout(() => { clearInterval(timer); ws.close(); }, DURATION);
}));

// 3. HTTP traffic while the sockets are busy: 50 concurrent loops over typical API calls.
const httpLat = []; const stopAt = Date.now() + DURATION - 2000;
const loops = Array.from({ length: process.env.HTTP === '0' ? 0 : 50 }, async (_, k) => {
  while (Date.now() < stopAt) {
    const i = Math.floor(Math.random() * tokens.length), pid = projects[Math.floor(i / G)];
    const url = [`/api/me`, `/api/projects`, `/api/projects/${pid}`, `/api/projects/${pid}/tree`][k % 4];
    const t = performance.now();
    const r = await fetch(BASE + url, { headers: hdr(i) }).catch(() => null);
    if (!r || !r.ok) errors.http++; else await r.arrayBuffer();
    httpLat.push(performance.now() - t);
  }
});
const sampler = setInterval(() => console.log(`open=${open} sent=${sent} recv=${received} p50=${pct(lat, .5)}ms p99=${pct(lat, .99)}ms http=${httpLat.length}`), 10000);
await Promise.all([...clients, ...loops]);
clearInterval(sampler);
console.log(JSON.stringify({ clients: tokens.length, updatesSent: sent, updatesDelivered: received,
  fanoutMs: { p50: pct(lat, .5), p95: pct(lat, .95), p99: pct(lat, .99), max: pct(lat, 1) },
  http: { requests: httpLat.length, rps: Math.round(httpLat.length / ((DURATION - 2000) / 1000)), p50: +pct(httpLat, .5).toFixed(2), p99: +pct(httpLat, .99).toFixed(2) },
  errors }, null, 1));
