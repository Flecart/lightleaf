# Scaling Lightleaf and serving it publicly

Lightleaf is meant to stay a **monolith**: one process owns the collaboration rooms, the SQLite database and the
project repositories. This document explains where the costs are, what the single process already does to keep
them small, and how to grow past one machine without turning it into a fleet of microservices.

## 1. Where the costs are

For 1000 concurrent users, the measured costs of everything except compiling are small (see
[`tools/loadtest`](../tools/loadtest/README.md)):

| Work | Cost at 1000 concurrent editors | Why it is cheap |
|---|---|---|
| Real-time editing (Yjs over WebSocket) | p99 fan-out 6 ms, about 0.5 core | One in-memory `yrs` doc per open file; updates are relayed as-is to the subscribers of that file only |
| API requests | ~4000 req/s on a few cores | SQLite in WAL mode, a pool of connections, cached statements, no ORM |
| Static assets | no CPU | Compressed once at startup (brotli 11 + gzip), served from memory, cached immutably |
| Memory | ~220 MB | Only open documents are in memory, and they are evicted when idle |
| **Compiling** | **dominant** | see below |

**Compile budget.** Assume 20% of users are actively typing with auto-compile on. That gives 200 editors, each
triggering a compile after a pause in typing, roughly every 10 s: about 20 compile requests/s. Compile coalescing
merges requests for the same document that arrive while a compile is queued or running. With five people per
project, about 8 compiles/s actually run. At 0.3–1 CPU-second per `pdflatex` run, **3–8 cores are busy compiling
all the time.** Size `LIGHTLEAF_MAX_COMPILES` to the number of cores minus one or two, and keep those cores for
compiling.

## 2. What the monolith already does

- **Compile coalescing**: a request that waited for a compile which started after it arrived reuses that result.
- **Per-user fairness**: at most `LIGHTLEAF_COMPILES_PER_USER` running/queued and `LIGHTLEAF_COMPILES_PER_MINUTE`
  executed per user; the global queue is a fair FIFO semaphore.
- **Bounded memory per client**: WebSocket outboxes are capped; slow clients are disconnected and resync on
  reconnect.
- **Connection pool** for SQLite (WAL: readers never block on the writer).
- **No wasted compression**: PDFs, zips, images and git packs are sent as-is; assets are pre-compressed.
- **Housekeeping**: idle docs and rooms are evicted, unused locks pruned, expired/idle sessions deleted.
- **Observability**: `/metrics` (Prometheus) and `LIGHTLEAF_LOG_FORMAT=json`.

## 3. Deployment designs

### A. One machine (recommended up to roughly 1–2k concurrent users)

```
            ┌─────────── one VM (e.g. 16 vCPU / 32 GB) ───────────┐
 users ───► │ Caddy (TLS, HTTP/2, WebSockets)                      │
            │   └─► lightleaf (single process)                      │
            │         ├─ SQLite (WAL) ──► Litestream ──► S3         │
            │         ├─ data/projects/* (git) ──► restic ──► S3    │
            │         └─ compilers (bwrap sandbox, no network)      │
            │ Prometheus agent ──► Grafana / alerting               │
            └──────────────────────────────────────────────────────┘
```

- **Maintenance:** one binary, one systemd unit (or the provided compose file); upgrades are
  `pull, build, restart` and take about one second of downtime (browsers reconnect and resync automatically).
- **Backups:** [Litestream](https://litestream.io) streams SQLite to object storage continuously; project repos
  are plain git, backed up with restic/borg every few minutes.
- **Settings:** `LIGHTLEAF_TRUST_PROXY=true`, `LIGHTLEAF_PORT=127.0.0.1:8080`, `LIGHTLEAF_SECURE_COOKIES=true`,
  `LIGHTLEAF_MAX_COMPILES=<cores-2>`, quotas set, `LIGHTLEAF_COMPILE_WRAPPER` set to a bwrap/nsjail command line.

This handles the target load with large headroom everywhere except compile CPU, which is the next design.

### B. Monolith + stateless compile workers (the first step past one machine)

Compiling is the only part that needs to scale out, and it is the only part that is a pure function:
*(project files) → (PDF, log, synctex)*. So it is the part to move out:

```
 lightleaf (editor, rooms, DB, git) ──HTTP──► compile worker 1  (sandboxed, no secrets, no DB)
                                     ├──────► compile worker 2
                                     └──────► compile worker N
```

- The worker is the same binary started as `lightleaf compile-worker`; the compile code already exists in
  `compile.rs`.
- **Sticky routing by project id** (rendezvous hashing), so a worker keeps that project's build directory and
  aux files warm, and reruns stay incremental.
- **Content-addressed sync:** the main node sends a manifest `{path: sha256}`, the worker replies with the hashes
  it is missing, and only those files are uploaded. A keystroke-sized edit means a few KB per compile.
- Workers hold no credentials and can run on cheap or spot machines, inside gVisor or Firecracker, with no
  network. A worker that dies only costs a retry.
- The main node keeps `LIGHTLEAF_MAX_COMPILES` as the global concurrency limit, now spread over the workers.

Estimated effort: a `CompileBackend` trait (local / remote), about 400 lines for the worker endpoint and the
manifest sync. Nothing else changes.

### C. Sharded monolith (only past roughly 5–10k concurrent users)

Run several identical Lightleaf nodes, each **owning** a subset of projects:

```
              ┌── node A: projects with hash(id) in shard A (rooms + repos + builds)
 router ──────┼── node B: …
 (Caddy/      └── node C: …
  tiny proxy)        └── shared account DB (one SQLite "accounts" node, or Postgres)
```

- The router sends `/api/projects/{id}/*`, the project WebSocket and `/git/{id}/*` to the owning node; account
  endpoints go to any node.
- Collaboration rooms stay **in-process** on the owner, so there is no Redis pub/sub and no cross-node Yjs
  merging. This is the property that keeps the system simple and fast.
- Rebalancing a project means: flush it, `rsync` its repo, update the shard map, close the room (clients
  reconnect to the new owner).
- Account data (users, sessions, memberships) is the only shared state; move it to Postgres at this stage.

### What not to do

- **Do not split into microservices** (auth service, file service, collab service). Every edit and compile would
  cross the network several times, and the operational cost would grow faster than the load.
- **Do not put Yjs updates through Redis/NATS** to make every node able to serve every project. Sticky ownership
  (design C) gets the same scale with none of the consistency problems.
- **Do not move to Kubernetes before you need design C.** A VM with systemd is easier to run, debug and back up.

## 4. Public-service checklist

Things a public, open-signup instance needs beyond a private one. ✅ marks what is already in place.

- ✅ Rate-limited, timing-safe login; session management and revocation; strict CSP and cross-site protection
- ✅ Sandboxed serving of uploaded files; per-user compile limits; project and size quotas
- ✅ Metrics and structured logs
- ☐ **Email verification and password reset by email** (needs an SMTP setting); invite links for collaborators
- ☐ **Bot protection on sign-up** (proof-of-work or hCaptcha/Turnstile) when `LIGHTLEAF_SIGNUP=open`
- ☐ **Compiler sandbox by default** (bwrap/nsjail with no network and a read-only root). LuaLaTeX can read files
  outside the project without it.
- ☐ Quotas also on git pushes and editor content; storage accounting including git history
- ☐ Account self-deletion and data export (GDPR), terms of service and privacy pages
- ☐ OIDC login (Google/GitHub/university SSO) and optional TOTP 2FA
- ☐ Admin tools: suspend user, see largest projects/top compilers (from `/metrics` and the DB)

## 5. Hooks for a free / Pro model

The limits introduced for public serving are the natural plan boundaries. Today they are global environment
variables. The next step is a `plans` table (`free`, `pro`, …) with these columns, and a `plan` column on `users`:

| Limit | Free (example) | Pro (example) |
|---|---|---|
| `max_projects` | 10 | unlimited |
| `max_project_mb` | 100 | 2000 |
| `compiles_per_minute` | 10 | 60 |
| `compile_timeout_secs` | 30 | 240 |
| `max_collaborators_per_project` | 3 | unlimited |
| git sync with external remotes | – | ✓ |

Every check already goes through one place (`Limits` / `Config`), so making them per-plan only changes where the
numbers come from. Paid-only *features* (as opposed to limits) can live in a separately licensed module; the
contribution terms in `CONTRIBUTING.md` keep that possible.
