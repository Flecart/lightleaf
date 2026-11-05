# 🍃 Lightleaf

[![CI](https://github.com/Flecart/lightleaf/actions/workflows/ci.yml/badge.svg)](https://github.com/Flecart/lightleaf/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

**A fast, lightweight, self-hosted collaborative LaTeX & Typst editor — with real git integration.**

Lightleaf gives you the Overleaf-style workflow (file tree · editor · live PDF preview · real-time
collaboration · history) in a **single ~10 MB Rust binary** with a SQLite database. Idle projects use no
memory, and a small VPS or a Raspberry Pi is enough to run it.

| | Overleaf CE | Lightleaf |
|---|---|---|
| Services to run | Node services + MongoDB + Redis | **one binary** (+ git + your TeX/Typst) |
| Idle RAM | ~1–2 GB | **~30–50 MB** |
| Git clone / push | Server Pro only | ✅ built in |
| Typst | ❌ | ✅ |
| GitHub/GitLab sync | Server Pro only | ✅ (any https remote) |

## Features

- **Overleaf-like UI** — file tree with drag & drop, CodeMirror 6 editor, PDF preview, resizable panes, editor/PDF/split layouts, light & dark themes.
- **Real-time collaboration** — conflict-free editing with [Yjs](https://yjs.dev) CRDTs (server side: [`yrs`](https://github.com/y-crdt/y-crdt)), live cursors and selections, presence avatars, project chat.
- **LaTeX** — `pdflatex`, `xelatex`, `lualatex` (through `latexmk` when installed, otherwise an automatic bibtex/biber/makeindex rerun pipeline) or `tectonic`.
- **Typst** — first-class support, with syntax highlighting and an outline.
- **Error log** — parsed errors, warnings and bad boxes, clickable to jump to the line; inline diagnostics in the editor.
- **SyncTeX** — double-click the PDF to jump to the source, and jump from the source to the PDF.
- **Auto-compile**, LaTeX autocomplete (commands, environments, `\ref` labels, `\cite` keys from your `.bib` files), file outline, browser spell-check.
- **Git integration**
  - every project *is* a git repository: `git clone http://your-host/git/<project-id>`
  - pushes go live for everyone who has the project open
  - web edits are committed automatically (attributed to whoever made them, with `Co-authored-by` trailers) whenever someone pulls
  - **History** panel with diffs, labelled versions, and restoring a single file or the whole project
  - push/pull to an external remote (GitHub, GitLab, Gitea, …)
- **Overleaf-compatible zips** — upload a zip from Overleaf's *Download → Source* (or arXiv, or any project folder). The main document is found using Overleaf's rules, and the TeX engine is detected from `% !TEX program`, `latexmkrc` or packages such as `fontspec`. Exports use Overleaf's layout, so they upload straight back to Overleaf.
- **Sharing** — invite collaborators as editors or viewers.
- Zip import/export (drag a zip onto the dashboard), project copy, image/PDF previews, personal git access tokens, simple admin panel.

## Quick start

### Docker (recommended)

```bash
git clone https://github.com/Flecart/lightleaf && cd lightleaf
cp .env.example .env              # adjust settings
docker compose up -d --build      # → http://localhost:8080
```

Then create your administrator account **on the server** (sign-up is closed by default, and the admin account can never be
claimed over the network):

```bash
docker compose exec lightleaf lightleaf user add --admin you@example.com   # prints a random password
```

Alternatively set `LIGHTLEAF_ADMIN_EMAIL`/`LIGHTLEAF_ADMIN_PASSWORD` in `.env`. See [Accounts and sign-up](#accounts-and-sign-up).

**HTTPS with automatic certificates.** Point DNS at the server and set in `.env`:
`LIGHTLEAF_DOMAIN`, `LIGHTLEAF_SECURE_COOKIES=true`, `LIGHTLEAF_TRUST_PROXY=true` and `LIGHTLEAF_PORT=127.0.0.1:8080`
(so the app is only reachable through the proxy), then run:

```bash
docker compose --profile https up -d --build
```

This starts a bundled Caddy proxy on ports 80/443 that fetches Let's Encrypt certificates and proxies WebSockets.

**What the image contains:** Lightleaf, git, Typst, and TeX Live (recommended + extra + science + recommended fonts + XeLaTeX/LuaLaTeX + biber + latexmk).
The default image is about 1.5 GB. Add `texlive-fonts-extra` to `TEX_PACKAGES` if you need the big font collection.
For a much smaller image, pick fewer TeX packages:

```bash
docker build --build-arg TEX_PACKAGES="texlive-latex-recommended latexmk" -t lightleaf:slim .
LIGHTLEAF_IMAGE=lightleaf:slim docker compose up -d
```

**Hardening:** the container runs as an unprivileged user with all capabilities dropped, `no-new-privileges`, a pids limit
and a memory limit. `tini` is PID 1, so killed compiler processes are reaped.

**Backups:** everything (SQLite database + one git repository per project) lives in the `lightleaf-data` volume:

```bash
docker run --rm -v lightleaf_lightleaf-data:/data -v "$PWD":/backup debian:bookworm-slim \
  tar czf /backup/lightleaf-backup.tgz -C /data .
```

**Upgrading:** `git pull && docker compose up -d --build`. Pushing a `v*` tag also publishes a multi-arch
(amd64/arm64) image to GHCR through `.github/workflows/docker.yml`.

### Bare metal

Requirements: `git`, and whichever compilers you want (`pdflatex`/`xelatex`/`lualatex`, `latexmk`, `tectonic`, `typst`, `synctex`). Lightleaf detects them on start-up.

```bash
# build
(cd frontend && npm install && npm run build)
cargo build --release
# run
LIGHTLEAF_DATA=/var/lib/lightleaf ./target/release/lightleaf
```

Put it behind a reverse proxy for TLS. The proxy has to forward WebSockets for `/api/projects/*/ws`. With Caddy that is just:

```
latex.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

## Configuration

All configuration is done with environment variables:

| Variable | Default | Description |
|---|---|---|
| `LIGHTLEAF_BIND` | `0.0.0.0:8080` | Listen address |
| `LIGHTLEAF_DATA` | `./data` | Data directory (SQLite DB, project repos, build dirs) |
| `LIGHTLEAF_SIGNUP` | `closed` | `closed` = no self-registration (admins create accounts) · `local` = self-registration only from the server machine itself · `open` = anyone can register |
| `LIGHTLEAF_ADMIN_EMAIL` / `LIGHTLEAF_ADMIN_PASSWORD` | – | Create an admin account on start-up if it doesn't exist |
| `LIGHTLEAF_MAX_COMPILES` | `min(cpus, 4)` | Maximum concurrent compiles (the rest are queued) |
| `LIGHTLEAF_COMPILES_PER_USER` | `2` | Compiles one user may have running or queued at once |
| `LIGHTLEAF_COMPILES_PER_MINUTE` | `30` | Compiles one user may run per minute (requests served from a coalesced compile do not count) |
| `LIGHTLEAF_COMPILE_TIMEOUT` | `90` | Seconds before a compile is killed |
| `LIGHTLEAF_COMPILE_WRAPPER` | – | Command prefix used to sandbox compilers, e.g. `firejail --quiet --net=none` |
| `LIGHTLEAF_MAX_UPLOAD_MB` | `50` | Max upload size |
| `LIGHTLEAF_MAX_PROJECTS` | `0` (unlimited) | Projects one user may own |
| `LIGHTLEAF_MAX_PROJECT_MB` | `0` (unlimited) | Maximum size of a project's files (excluding git history), enforced on uploads and zip imports |
| `LIGHTLEAF_SECURE_COOKIES` | `false` | Set to `true` when served over HTTPS (also enables HSTS) |
| `LIGHTLEAF_TRUST_PROXY` | `false` | Use the client IP from `X-Forwarded-For` (set to `true` **only** behind your own reverse proxy) |
| `LIGHTLEAF_SESSION_DAYS` | `30` | Maximum session lifetime |
| `LIGHTLEAF_SESSION_IDLE_DAYS` | `14` | Sessions unused for this long are logged out |
| `LIGHTLEAF_SOURCE_URL` | repository URL | "Source code" link shown in the UI (AGPL §13 — point it at your fork if you modify Lightleaf) |
| `LIGHTLEAF_SITE_NAME` | `Lightleaf` | Name shown in the UI |
| `LIGHTLEAF_METRICS_TOKEN` | – | Allows scraping `/metrics` from other machines with `Authorization: Bearer <token>` (it is always reachable from the server itself) |
| `LIGHTLEAF_LOG_FORMAT` | `text` | `json` writes one JSON object per log line |
| `RUST_LOG` | `lightleaf=info` | Log level |

## Accounts and sign-up

Who can create an account is set with `LIGHTLEAF_SIGNUP`:

| Mode | Who can register in the browser |
|---|---|
| `closed` (default) | Nobody. Admins create accounts in **Admin** or with the CLI below. |
| `local` | Only requests made on the server machine itself, e.g. through an SSH tunnel: `ssh -L 8080:localhost:8080 server`, then open `http://localhost:8080`. A request counts as local when it comes from a loopback address, carries no proxy headers (`X-Forwarded-For`, `Forwarded`, …) and is addressed to `localhost`. A reverse proxy on the same machine must therefore add `X-Forwarded-For` (Caddy and Traefik always do; with nginx add `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`). In Docker, requests never look local; use the CLI. |
| `open` | Anyone who can reach the server. |

The **first** account becomes the administrator, so it can only be created from the server (CLI, `LIGHTLEAF_ADMIN_*`
variables, or a local request), never by whoever reaches a fresh install first.

### Command-line account management

Run on the server (in Docker, prefix with `docker compose exec lightleaf`). It reads the same `LIGHTLEAF_DATA` as the
server and is safe to use while the server is running.

```bash
lightleaf user add <email> [--name NAME] [--admin] [--password-stdin]   # create an account
lightleaf user passwd <email> [--password-stdin]                        # reset a password (logs the user out everywhere)
lightleaf user admin <email> on|off                                     # grant/remove admin rights
lightleaf user logout <email>                                           # end all of the user's sessions
lightleaf user list                                                     # list accounts
```

Without `--password-stdin` a random 20-character password is generated and printed. With it, the password is read from
standard input: `echo 'correct horse battery' | lightleaf user add bob@example.com --password-stdin`.

## Using git

1. Create a token in **Account & git tokens**.
2. Open a project → **Git** to copy the clone URL.
3. Clone it, using the token as the password (any username):

```bash
git clone http://localhost:8080/git/9f0c…e1 my-paper
cd my-paper
# edit, commit
git push            # collaborators see the change instantly in their editor
git pull            # includes everything typed in the web editor
```

Tip: `git config credential.helper store` saves the token so you are not asked again.

**External remotes:** in the Git dialog the project owner can set a remote such as
`https://<token>@github.com/you/paper.git` and then push or pull with one click. Merge conflicts are not resolved
automatically — if a pull conflicts it is aborted and nothing changes.

## Architecture

```
┌──────────── browser ─────────────┐        ┌────────────────── lightleaf (Rust) ───────────────────┐
│ Preact UI · CodeMirror 6 · Yjs   │  HTTP  │ axum router ── auth (argon2, sessions) ── SQLite (WAL)│
│ PDF.js viewer                    │◄──────►│   │                                                    │
│                                  │   WS   │   ├─ collab hub: one room per open project,           │
│                                  │◄──────►│   │   one yrs CRDT doc per open file (in-memory only  │
└──────────────────────────────────┘        │   │   while in use; flushed to disk every 2 s)        │
┌──────────── git client ──────────┐  HTTP  │   ├─ git smart-HTTP (git upload-pack/receive-pack)    │
│ git clone / pull / push          │◄──────►│   ├─ compile queue (semaphore) → latexmk / typst …    │
└──────────────────────────────────┘        │   └─ embedded static frontend (rust-embed)            │
                                            └─────────────── data/projects/<id>  (git working trees)┘
```

- **Storage is plain files.** Each project is an ordinary git repository under `data/projects/<id>`, so backups are just `rsync`/`restic` of the data directory.
- **Builds are isolated from sources.** Sources are mirrored incrementally to `data/build/<id>`, so aux files never end up in git and reruns stay fast.
- **The WebSocket protocol is small and binary:** `[type u8][path_len u16][path][payload]`, carrying Yjs sync, update and awareness messages. Each document has an *epoch*, so clients resync safely if the server reloads a document (for example after a `git push`).
- **Concurrency.** A per-project async lock serialises disk-level operations (flush, commit, push, restore, build sync), so a `git push` never races with editor writes.

### Project layout

```
src/
  main.rs       router & startup           collab.rs   real-time CRDT rooms
  reviewer.rs   optional proxy to the AI review service (reviewer/)
  auth.rs       users, sessions, tokens    compile.rs  LaTeX/Typst, log parsing, SyncTeX
  projects.rs   projects, sharing, chat    git.rs      smart HTTP, history, remotes
  files.rs      file tree, upload, zip     assets.rs   embedded frontend
  overleaf.rs   Overleaf zip import/export, main-file & engine detection
reviewer/       lightleaf-reviewer: separate AI review service (own crate, image and README)
frontend/src/
  editor/       EditorPage, CodeEditor, PdfViewer, FileTree, Panels, socket (Yjs client), lang
  review/       AI review dialog (lazy-loaded, isolated by an error boundary)
  pages/        dashboard, auth, settings, admin
```

## Development

```bash
cargo run                                  # backend on :8080 (serves the last frontend build)
cd frontend && npm install && npm run dev  # Vite on :5173, proxies /api and /git to :8080
cargo test
```

## AI review (optional)

A separate service, [`lightleaf-reviewer`](reviewer/README.md), can review your paper with a team of AI reviewers
(abstract, method, results, writing, LaTeX, figures, venue fit, …). It writes their comments **into the LaTeX
source** as `\AI[Results, warning]{…}`, so they show up in the PDF next to the text they're about. There's
no comment database: comments are git history, can be reverted, removed in one click, or hidden with
`\usepackage[hide]{lightleaf-comments}`.

It also has a **reference check** that verifies every cited reference against Semantic Scholar and flags
fabricated, wrong or undefined ones next to the `\cite`. The default review methodology is a port of
[PaperMentor](https://github.com/jiarui-liu/overleaf) with its writing guides. Claude and OpenAI-compatible models
are supported.

It's fully isolated: a separate process and container that touches projects only through git, using a short-lived
token scoped to one project. Leave `LIGHTLEAF_REVIEWER_URL` unset and the feature doesn't exist. If the
reviewer is down, only the Review dialog notices. Enable it with `docker compose --profile review up -d` (see
[reviewer/README.md](reviewer/README.md) for every deployment option and setting).

## Scaling

A single process comfortably serves about 1000 concurrent editors; compiling is the only real CPU cost. See
[docs/SCALING.md](docs/SCALING.md) for the capacity model, deployment designs for a public instance (one machine →
stateless compile workers → sharded monolith), and [tools/loadtest](tools/loadtest/README.md) to measure your own
server.

## Security notes

- TeX runs with restricted shell escape (`shell_escape=p`) and paranoid file access (`openin_any=p`, `openout_any=p`), so pdfLaTeX/XeLaTeX documents cannot read or write files outside the project. **LuaLaTeX is the exception:** its Lua interpreter can read files on its own, so the read restriction does not apply to it. Typst is sandboxed to the project root.
- For untrusted users, also sandbox the compilers with `LIGHTLEAF_COMPILE_WRAPPER` (for example `firejail` or `bwrap` with networking disabled), or run the container with a read-only root filesystem.
- Passwords are hashed with Argon2id (at most 4 hashes run at once, so a login flood cannot exhaust memory). Session and git tokens are stored only as SHA-256 hashes.
- Logins are rate limited: 10 failed attempts per account per 15 minutes, and 30 failed logins or registrations per client IP per 10 minutes. Unknown emails take as long to reject as wrong passwords, so accounts cannot be discovered by timing.
- Sessions expire after `LIGHTLEAF_SESSION_DAYS`, or earlier after `LIGHTLEAF_SESSION_IDLE_DAYS` without use. Users can see and revoke their sessions in **Account settings**. Changing or resetting a password logs out every other session.
- State-changing API requests from other sites are rejected (`Origin` / `Sec-Fetch-Site` checks on top of `SameSite=Lax` cookies). The app is served with a strict Content-Security-Policy, `X-Frame-Options`, `nosniff` and, over HTTPS, HSTS.
- Uploaded files are served sandboxed (`CSP: sandbox`), and HTML/XML/JS uploads are served as plain text, so a collaborator cannot run scripts in your session by uploading a malicious file.
- Repositories check out symlinks as plain files (`core.symlinks=false`), and git hooks are disabled.
- External remotes are limited to `http(s)://`.

## Roadmap ideas

Track changes & comments · rich-text mode · link sharing · invite links · OIDC/LDAP login · two-factor authentication · per-project compile images · Typst live preview (incremental) · word count · vim/emacs keybindings.

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Please report security problems privately as described in [SECURITY.md](SECURITY.md).

## License

Lightleaf is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only). You can use, modify
and self-host it freely. If you run a modified version as a network service, you must offer its source code to your
users (set `LIGHTLEAF_SOURCE_URL` to point at it).

For licensing under other terms (for example, to embed Lightleaf in a proprietary product without the AGPL's
obligations), contact the maintainer.
