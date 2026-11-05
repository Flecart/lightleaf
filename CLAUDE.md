# Lightleaf: notes for coding agents

Self-hosted collaborative LaTeX/Typst editor: a single Rust binary (axum + yrs + SQLite) with an embedded
Preact/CodeMirror frontend. AGPL-3.0-only. User-facing docs are in `README.md`; scaling and deployment design is in
`docs/SCALING.md`.

## Build, run, check

```bash
(cd frontend && npm install && npm run build)   # REQUIRED before cargo build: rust-embed embeds frontend/dist
cargo build                                     # debug; use --release for any performance measurement
cargo test
cargo fmt --check && cargo clippy --all-targets # CI requires both clean (zero warnings)
(cd frontend && npx tsc --noEmit)               # frontend type-check (also part of `npm run build`)
```

Run a throwaway dev server (keep data out of the repo; `/data` is gitignored but use a temp dir anyway):

```bash
export LIGHTLEAF_DATA=/tmp/ll-dev LIGHTLEAF_BIND=127.0.0.1:18080
./target/debug/lightleaf &            # server
./target/debug/lightleaf help         # CLI usage
```

Stop it with `kill <pid>` of the server process. **Do not use `pkill -f lightleaf` or `pkill -f <script name>`.**
The pattern also matches the agent's own shell command line and kills it (exit code 144).

## The `lightleaf` CLI (account management)

The binary doubles as an admin CLI. It opens the database at `$LIGHTLEAF_DATA/lightleaf.db` (same env as the
server), needs no running server, and is safe to run while one is up (SQLite WAL + busy timeout). Any first
argument other than `serve` selects the CLI.

| Command | Effect |
|---|---|
| `lightleaf user add <email> [--name NAME] [--admin] [--password-stdin]` | Create an account. Name defaults to the email's local part. Prints a random 20-char password unless `--password-stdin`. |
| `lightleaf user passwd <email> [--password-stdin]` | Set a new password (random unless stdin) and **revoke all of the user's sessions**. |
| `lightleaf user admin <email> on\|off` | Grant/remove admin rights. |
| `lightleaf user logout <email>` | Delete all sessions of the user. |
| `lightleaf user list` | `email  name  [admin]`, one per line. |
| `lightleaf help`, `lightleaf --version` | Usage / version. |

Passwords: 8–1024 characters. `--password-stdin` reads all of stdin and strips the trailing newline.
Exit code is non-zero on error (unknown email, duplicate account, weak password).

### Recipes

```bash
# Admin + a normal user with known passwords for testing
echo 'adminpass123' | lightleaf user add admin@test.dev --admin --password-stdin
echo 'alicepass123' | lightleaf user add alice@test.dev --name Alice --password-stdin

# Log in with curl and call the API (cookie jar). Requests from 127.0.0.1 count as "local".
curl -s -c /tmp/jar -H 'Content-Type: application/json' \
  -d '{"email":"admin@test.dev","password":"adminpass123"}' http://127.0.0.1:18080/api/auth/login
curl -s -b /tmp/jar http://127.0.0.1:18080/api/me

# In Docker
docker compose exec lightleaf lightleaf user add --admin you@example.com
```

Behaviour to remember when testing:
- Sign-up defaults to `closed`. On a fresh database, browser registration only works for a **local** request
  (loopback peer, no `X-Forwarded-*`/`Forwarded`/`X-Real-IP` headers, `Host` = localhost/127.0.0.1/[::1]). To
  simulate a remote client from curl, add `-H 'X-Forwarded-For: 1.2.3.4'`.
- Login rate limits are in memory: 10 failures per email / 15 min, 30 per IP / 10 min. Restart the server to
  reset them during tests.
- Mutating `/api` requests with a foreign `Origin` or `Sec-Fetch-Site: cross-site` get 403.
- `/metrics` answers local requests (or `Authorization: Bearer $LIGHTLEAF_METRICS_TOKEN`).

## Database

Schema is in `src/db.rs` (`CREATE TABLE IF NOT EXISTS`). **There are no migrations yet.** The project is
pre-release and the maintainer prefers recreating the DB. After a schema change, delete the data dir
(`rm -rf $LIGHTLEAF_DATA`) instead of writing migration code. All DB access goes through
`AppState::db(|conn| ...)`, which runs on the blocking pool with a connection from a small pool. Don't hold
explicit transactions across awaits; use `prepare_cached` for hot queries.

## Code map

```
src/main.rs      startup, CLI dispatch, router, middleware stack (CSRF, security headers, compression)
src/cli.rs       `lightleaf user ...` commands
src/auth.rs      users, sessions (idle timeout, revocation), login/register, tokens, admin endpoints
src/security.rs  client IP / local-request detection, rate limiters, per-user compile slots, CSRF, HSTS
src/config.rs    every LIGHTLEAF_* env var (keep README's table and .env.example in sync when adding one)
src/state.rs     AppState: DB pool, locks, compile coalescing cache, limits, metrics
src/collab.rs    WebSocket rooms, yrs docs, bounded outboxes, flusher
src/compile.rs   LaTeX/Typst runs, log parsing, SyncTeX, coalescing + per-user limits
src/files.rs     file tree, upload (quota), sandboxed file serving
src/projects.rs  projects, members, chat, zip import (quota), copy
src/git.rs       smart-HTTP git, history, external remotes
src/metrics.rs   Prometheus /metrics
src/reviewer.rs  optional proxy to the AI reviewer (only coupling point)
src/assets.rs    embedded, pre-compressed frontend + CSP
tools/loadtest/  1000-user WebSocket + HTTP load test (see its README)
```

## AI reviewer (`reviewer/`)

A separate workspace crate and binary, `lightleaf-reviewer`. Keep it **decoupled**: core may only talk to it through
`src/reviewer.rs` (HTTP with timeouts; feature off when `LIGHTLEAF_REVIEWER_URL` is unset), and the reviewer may
only touch projects through git with the project-scoped token core issues. Never import reviewer code into core
or give the reviewer DB access. Frontend code for it stays in `frontend/src/review/` behind the lazy
`ReviewEntry` + error boundary.

```bash
cargo build -p lightleaf-reviewer && cargo test -p lightleaf-reviewer
# Local end-to-end run with no API cost: start a stub Messages API that returns structured outputs, then
LIGHTLEAF_REVIEWER_SECRET=<16+ chars> ANTHROPIC_API_KEY=fake ANTHROPIC_BASE_URL=http://127.0.0.1:<stub> \
  LIGHTLEAF_REVIEWER_BIND=127.0.0.1:18090 ./target/debug/lightleaf-reviewer &
LIGHTLEAF_REVIEWER_URL=http://127.0.0.1:18090 LIGHTLEAF_REVIEWER_SECRET=<same> ./target/debug/lightleaf &
# then: POST /api/projects/<id>/review, poll GET /api/projects/<id>/review, check `git log` in the project dir
```

- Comments are `\AI[<Agent>, <severity>]{text}` (macro name: `LIGHTLEAF_REVIEW_MACRO`). The macro is reserved
  for review comments: "Clear" removes every `\AI[…]{…}` and `\AI{…}` (but not longer commands like `\AIbox`).
- Insertion rules (`reviewer/src/latex.rs::insertion_point`) are the riskiest code: any change needs a unit test,
  and a compile check of the result (`pdflatex` on a document that uses the macro in titles, captions,
  footnotes, lists and math, with each package option).
- `reviewer/tex/lightleaf-comments.sty` is written into user projects. Keep it dependency-light (xcolor only)
  and backwards compatible.
- Claude calls are raw HTTP (`reviewer/src/claude.rs`): structured outputs, `claude-opus-5-5`,
  `fallbacks: "default"`. The paper is the cached prefix shared by all agents, so keep the system prompt and
  document block identical across a job's calls.

## Conventions

- Commits: one logical change per commit, with an explanatory body (why, not only what). Don't mix refactors,
  features and docs. When one file holds several concerns, stage them separately.
- New env var → `config.rs`, README configuration table, `.env.example`.
- New user-visible security behaviour → README "Security notes".
- Keep dependencies light (see CONTRIBUTING.md design principles); prefer std/tokio over new crates.
