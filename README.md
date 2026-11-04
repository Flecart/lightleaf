# 🍃 Lightleaf

[![CI](https://github.com/Flecart/lightleaf/actions/workflows/ci.yml/badge.svg)](https://github.com/Flecart/lightleaf/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

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

The first account you register becomes the admin (or set `LIGHTLEAF_ADMIN_EMAIL`/`LIGHTLEAF_ADMIN_PASSWORD`).

**HTTPS with automatic certificates.** Point DNS at the server, set `LIGHTLEAF_DOMAIN` and `LIGHTLEAF_SECURE_COOKIES=true`
in `.env`, then run:

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
| `LIGHTLEAF_SIGNUP` | `open` | `closed` = only admins can create accounts (the first account can always be created) |
| `LIGHTLEAF_ADMIN_EMAIL` / `LIGHTLEAF_ADMIN_PASSWORD` | – | Create an admin account on start-up if it doesn't exist |
| `LIGHTLEAF_MAX_COMPILES` | `min(cpus, 4)` | Maximum concurrent compiles (the rest are queued) |
| `LIGHTLEAF_COMPILE_TIMEOUT` | `90` | Seconds before a compile is killed |
| `LIGHTLEAF_COMPILE_WRAPPER` | – | Command prefix used to sandbox compilers, e.g. `firejail --quiet --net=none` |
| `LIGHTLEAF_MAX_UPLOAD_MB` | `50` | Max upload size |
| `LIGHTLEAF_SECURE_COOKIES` | `false` | Set to `true` when served over HTTPS |
| `LIGHTLEAF_SITE_NAME` | `Lightleaf` | Name shown in the UI |
| `RUST_LOG` | `lightleaf=info` | Log level |

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
  auth.rs       users, sessions, tokens    compile.rs  LaTeX/Typst, log parsing, SyncTeX
  projects.rs   projects, sharing, chat    git.rs      smart HTTP, history, remotes
  files.rs      file tree, upload, zip     assets.rs   embedded frontend
  overleaf.rs   Overleaf zip import/export, main-file & engine detection
frontend/src/
  editor/       EditorPage, CodeEditor, PdfViewer, FileTree, Panels, socket (Yjs client), lang
  pages/        dashboard, auth, settings, admin
```

## Development

```bash
cargo run                                  # backend on :8080 (serves the last frontend build)
cd frontend && npm install && npm run dev  # Vite on :5173, proxies /api and /git to :8080
cargo test
```

## Security notes

- TeX runs with restricted shell escape (`shell_escape=p`) and paranoid file access (`openin_any=p`, `openout_any=p`), so pdfLaTeX/XeLaTeX documents cannot read or write files outside the project. **LuaLaTeX is the exception:** its Lua interpreter can read files on its own, so the read restriction does not apply to it. Typst is sandboxed to the project root.
- For untrusted users, also sandbox the compilers with `LIGHTLEAF_COMPILE_WRAPPER` (for example `firejail` or `bwrap` with networking disabled), or run the container with a read-only root filesystem.
- Passwords are hashed with Argon2id. Session and git tokens are stored only as SHA-256 hashes.
- Repositories check out symlinks as plain files (`core.symlinks=false`), and git hooks are disabled.
- External remotes are limited to `http(s)://`.

## Roadmap ideas

Track changes & comments · rich-text mode · link sharing · OIDC/LDAP login · per-project compile images · Typst live preview (incremental) · word count · vim/emacs keybindings.

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Please report security problems privately as described in [SECURITY.md](SECURITY.md).

## License

MIT — see [LICENSE](LICENSE).
