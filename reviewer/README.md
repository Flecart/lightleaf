# lightleaf-reviewer: AI review for Lightleaf

An optional service that reviews a LaTeX paper the way a careful co-author would, and writes its comments **into
the source**, right after the passage they are about:

```latex
We show that our method is always better. \ANG[AI: Results, critical]{"Always" is not supported: Table 2 shows two datasets where the baseline wins. Qualify the claim.}
```

The comments appear in the PDF (in colour, inline or in the margin). They are ordinary text in your project, so
there is no comment database: they live in git like everything else.

- **Your own comments** use the same macro: `\ANG{check this number}` or `\ANG[Alice]{…}`.
- **Hide them all** for submission: `\usepackage[hide]{lightleaf-comments}` (or `[margin]` to move them to the margin).
- **Remove the AI ones**: *Review → Remove AI comments* removes every `\ANG[AI…]{…}` and keeps yours. You can
  also revert the reviewer's commit in **History**.

## How it works

```
 Lightleaf ──(1) POST /v1/jobs: git URL + 1-hour token for this project only──► reviewer
     ▲                                                                          │ (2) git clone
     │                                                                          │ (3) review with Claude
     └────────────(4) git push: one commit "AI review: N comments"──────────────┘
```

1. In the editor, **Review → Start review** (optionally with a target venue and a focus). Lightleaf issues a git token
   that works only for this project and expires after an hour, and hands the job to the reviewer.
2. The reviewer clones the project, reads the main file and every `\input`/`\include`, and splits it into sections.
3. One call classifies the paper and its sections. Then up to 11 specialised reviewers run in parallel: abstract,
   introduction, related work, methods, results, conclusion, appendix, writing, LaTeX usage, figures and tables,
   and venue fit when a venue is given. Each returns comments anchored to an exact quote from the source.
   Comments whose quote cannot be found are dropped. Similar comments on the same passage are merged, and the most
   severe are kept (default limit: 40).
4. Each comment is inserted at the end of the line it refers to, moved out of anything it could break (math,
   tables, figures, verbatim, `\cite{…}` arguments, inline math, `%` comments). The reviewer adds
   `lightleaf-comments.sty` and one `\usepackage` line, commits as *Lightleaf Reviewer*, and pushes. If someone
   edited the project in the meantime, it re-applies the comments on top of the new version.

Everyone with the project open sees the comments arrive live, and the PDF recompiles.

## Isolation: why it can't break Lightleaf

- It is a **separate process** (separate binary, container and dependencies). Lightleaf talks to it through one
  module (`src/reviewer.rs`) with short timeouts. If the reviewer is down, slow or misconfigured, the Review dialog
  says so and nothing else is affected.
- It touches projects **only through git**, with a token scoped to one project and limited in time. It never sees
  Lightleaf's database, sessions or other projects. Its results are normal commits and can be reverted.
- Paper text is treated as data in the prompts, and comments are LaTeX-escaped before they are inserted, so a
  document can't trick the reviewer into writing LaTeX code into your source.
- It keeps no state: jobs are held in memory, and each checkout lives in a temporary directory that is deleted
  after the job.
- **Unplugging it:** stop the service and leave `LIGHTLEAF_REVIEWER_URL` empty. The Review button disappears.
  Comments already in projects stay valid LaTeX (they are defined by `lightleaf-comments.sty` in the project).

## Running it

### Docker Compose (same machine as Lightleaf)

In `.env`:

```bash
LIGHTLEAF_REVIEWER_URL=http://reviewer:8090
LIGHTLEAF_REVIEWER_GIT_URL=http://lightleaf:8080
LIGHTLEAF_REVIEWER_SECRET=<openssl rand -hex 32>
ANTHROPIC_API_KEY=sk-ant-...
```

```bash
docker compose --profile review up -d --build          # add --profile https for the Caddy proxy
```

The reviewer has no published ports. Only Lightleaf can reach it, on the compose network.

### Bare metal (systemd or a terminal)

```bash
cargo build --release -p lightleaf-reviewer
export LIGHTLEAF_REVIEWER_SECRET=$(openssl rand -hex 32)

# reviewer (listens on 127.0.0.1:8090 by default)
ANTHROPIC_API_KEY=sk-ant-... ./target/release/lightleaf-reviewer

# Lightleaf (git URL defaults to http://127.0.0.1:<its port>)
LIGHTLEAF_REVIEWER_URL=http://127.0.0.1:8090 ./target/release/lightleaf
```

### Separate machine

Run the reviewer where it can reach Lightleaf's `/git/` endpoint. Set `LIGHTLEAF_REVIEWER_BIND=0.0.0.0:8090` on
the reviewer, `LIGHTLEAF_REVIEWER_URL=http://<reviewer-host>:8090` and
`LIGHTLEAF_REVIEWER_GIT_URL=https://<your-lightleaf-domain>` on Lightleaf. The reviewer API is plain HTTP with a
shared secret, so keep it on a private network (VPN, VPC, WireGuard). Don't expose it to the internet.

## Configuration

Lightleaf side:

| Variable | Default | |
|---|---|---|
| `LIGHTLEAF_REVIEWER_URL` | – | `http://host:port` of the reviewer. Unset = feature off. |
| `LIGHTLEAF_REVIEWER_SECRET` | – | Shared secret (16+ characters), same value on both sides |
| `LIGHTLEAF_REVIEWER_GIT_URL` | `http://127.0.0.1:<port>` | How the reviewer reaches Lightleaf |
| `LIGHTLEAF_REVIEWER_TOKEN_TTL` | `3600` | Lifetime of the per-job git token, in seconds |

Reviewer side:

| Variable | Default | |
|---|---|---|
| `LIGHTLEAF_REVIEWER_SECRET` | – | Required |
| `ANTHROPIC_API_KEY` | – | Without it the service starts but refuses reviews (removing comments still works) |
| `LIGHTLEAF_REVIEWER_BIND` | `127.0.0.1:8090` (`0.0.0.0:8090` in Docker) | Listen address |
| `LIGHTLEAF_REVIEW_MODEL` | `claude-opus-5-5` | Claude model |
| `LIGHTLEAF_REVIEW_EFFORT` | `high` | `low` … `max`: depth versus cost |
| `LIGHTLEAF_REVIEW_MACRO` | `ANG` | Name of the comment command (letters only) |
| `LIGHTLEAF_REVIEW_MAX_COMMENTS` | `40` | Comments kept per review |
| `LIGHTLEAF_REVIEW_MAX_JOBS` | `2` | Reviews running at once (others queue) |
| `LIGHTLEAF_REVIEW_AGENT_CONCURRENCY` | `6` | Parallel model calls per review |
| `LIGHTLEAF_REVIEW_AGENT_TIMEOUT` | `600` | Seconds before one reviewer is abandoned |
| `LIGHTLEAF_REVIEW_MAX_INPUT_CHARS` | `800000` | Longer documents are refused, never silently truncated |
| `LIGHTLEAF_REVIEW_DISABLED_AGENTS` | – | Comma list, e.g. `latex_formatting,venue` |
| `LIGHTLEAF_REVIEW_PROMPTS_DIR` | – | Directory of `<name>.md` files that replace the built-in prompts in `reviewer/prompts/` |
| `LIGHTLEAF_REVIEW_FALLBACKS` | `true` | Send `fallbacks: "default"` (turn off for API gateways that reject it) |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | API endpoint (e.g. a gateway) |
| `LIGHTLEAF_REVIEW_WORKDIR` | system temp dir | Where checkouts are made |

**Cost.** The paper is sent once per call but cached: the first call writes the cache, and the parallel reviewers
read it at a fraction of the input price. A typical 10-page paper takes about 12 calls. Each job's token use is
shown in the Review dialog. Lower `LIGHTLEAF_REVIEW_EFFORT` or disable agents to reduce cost.

## Limits

- LaTeX only for now. Typst projects are refused with a clear message.
- Comments are anchored by exact quotes. If you rewrite a passage while a review is running, its comment is
  skipped (the dialog reports how many could not be placed).
- Quotas on reviews per user are not enforced yet. Use `LIGHTLEAF_REVIEW_MAX_JOBS` and your API key's limits.

## Development

```bash
cargo test -p lightleaf-reviewer        # LaTeX anchoring/insertion, dedup, API request/response handling
```

To try the full flow without API costs, point `ANTHROPIC_BASE_URL` at a stub that returns structured outputs.
Prompts are plain Markdown in `reviewer/prompts/`, and the agent table is in `src/pipeline.rs`.
