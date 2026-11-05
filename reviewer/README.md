# lightleaf-reviewer: AI review for Lightleaf

An optional service that reviews a LaTeX paper the way a careful co-author would, and writes its comments **into
the source**, right after the passage they are about:

```latex
We show that our method is always better. \AI[Results, critical]{"Always" is not supported: Table 2 shows two datasets where the baseline wins. Qualify the claim.}
```

The comments appear in the PDF (in colour, inline or in the margin). They are ordinary text in your project, so
there is no comment database: they live in git like everything else.

- **`\AI` is reserved for these comments.** Use a different command for your own notes (any name
  except `\AI`), so removing AI comments never touches them.
- **Hide them all** for submission: `\usepackage[hide]{lightleaf-comments}` (or `[margin]` to move them to the margin).
- **Remove the AI ones**: *Review → Remove AI comments* removes every `\AI[…]{…}`. You can
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
3. A **review pack** does the reviewing (see [Review packs](#review-packs)). With the default `papermentor` pack,
   one call classifies the paper type and every section. Then specialised reviewers run in parallel:
   - one per section category: abstract, introduction, related work, methods, results, conclusion, appendix;
   - one hybrid reviewer for each section that mixes two categories;
   - whole-paper reviewers for writing style, LaTeX and formatting, and figures and captions;
   - a paper-type reviewer and a venue reviewer, when a guide exists for them;
   - two rule-based checks: a code/data footnote at the end of the abstract, and an experimental-setup section
     placed before the results.

   Each reviewer returns comments anchored to a quote from the source (exact, or a close fuzzy match).
   Duplicates on the same passage are merged, and the most severe comments are kept (default limit: 40; in
   **strict mode** only issues a reviewer would raise, at most 30). The model is Claude (Anthropic API) or any
   model behind an OpenAI-compatible API, with structured JSON output in both cases.
4. Each comment is inserted at the end of the line it refers to, moved out of anything it could break (math,
   tables, figures, verbatim, `\cite{…}` arguments, inline math, `%` comments). The reviewer adds
   `lightleaf-comments.sty` and one `\usepackage` line, commits as *Lightleaf Reviewer*, and pushes. If someone
   edited the project in the meantime, it re-applies the comments on top of the new version.

Everyone with the project open sees the comments arrive live, and the PDF recompiles.

## Reference check

**Review → Check references** verifies every reference the paper cites against
[Semantic Scholar](https://www.semanticscholar.org/), using the DOI when there is one and the title otherwise.
It flags:
- references that look **fabricated** (no paper with a similar title);
- DOIs that are unknown or point to a **different paper**;
- entries whose **authors, year, venue or DOI disagree** with the record;
- keys that are **cited but not defined** in any `.bib` file;
- keys **defined twice**.

Each problem becomes a comment right after the first `\cite` of that key, so it shows in the PDF next to the
citation. The dialog lists all of them. Web pages and software entries without a DOI are not looked up.
"Not confirmed" (common for workshop papers and old work) is a warning, not an accusation.

The check needs no model credentials. A Semantic Scholar API key (`LIGHTLEAF_S2_API_KEY`, free on request) makes it
about three times faster. Without one, it uses the shared public rate limit.

## Review packs

A pack is a complete review methodology: prompts, writing guides, how sections are assigned, and which reviewers
run. Select it with `LIGHTLEAF_REVIEW_PACK`.

| Pack | What it is | License of its prompts |
|---|---|---|
| `papermentor` (default) | Port of the [PaperMentor / AI Tutor](https://github.com/jiarui-liu/overleaf) pipeline, with its prompts and 44 writing guides (venues, paper types, sections, figures, style) | Third-party AGPL-3.0, see `packs/papermentor/NOTICE.md` |
| `lightleaf` | A compact set of 11 reviewers with Lightleaf's own prompts | Lightleaf's own (AGPL-3.0-only) |

Use the `lightleaf` pack for any build distributed under other terms than the AGPL: the `papermentor` prompts are
not Lightleaf's to relicense. Pack files can be customised without rebuilding: put files with the same relative
path (e.g. `prompts/subagent.methods.preamble.md`, `skills/02_venues/neurips_2025.md`, or `methods.md` for the
`lightleaf` pack) in a directory and set `LIGHTLEAF_REVIEW_PROMPTS_DIR`.

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
| `LIGHTLEAF_REVIEW_PROVIDER` | auto | `anthropic` or `openai`. Auto: `anthropic` if `ANTHROPIC_API_KEY` is set, else `openai` if `OPENAI_API_KEY` or `OPENAI_BASE_URL` is set |
| `ANTHROPIC_API_KEY` | – | Anthropic credentials. Without credentials for the selected provider the service starts but refuses reviews (removing comments still works) |
| `OPENAI_API_KEY` | – | Key for OpenAI or an OpenAI-compatible server (sent as `Authorization: Bearer`) |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | Any OpenAI-compatible Chat Completions endpoint: OpenRouter, LiteLLM, vLLM, Ollama (`http://localhost:11434/v1`), … A local server needs no key |
| `LIGHTLEAF_REVIEW_OPENAI_REASONING_EFFORT` | – | Sent as `reasoning_effort` (only for reasoning models) |
| `LIGHTLEAF_REVIEW_OPENAI_MAX_TOKENS_FIELD` | `max_completion_tokens` | Use `max_tokens` for older compatible servers |
| `LIGHTLEAF_REVIEWER_BIND` | `127.0.0.1:8090` (`0.0.0.0:8090` in Docker) | Listen address |
| `LIGHTLEAF_REVIEW_MODEL` | `claude-opus-5-5` / `gpt-5.2-chat-latest` | Model, per provider |
| `LIGHTLEAF_REVIEW_EFFORT` | `high` | Anthropic only: `low` … `max`, depth versus cost |
| `LIGHTLEAF_REVIEW_MACRO` | `AI` | Name of the comment command (letters only) |
| `LIGHTLEAF_REVIEW_MAX_COMMENTS` | `40` | Comments kept per review |
| `LIGHTLEAF_REVIEW_MAX_JOBS` | `2` | Reviews running at once (others queue) |
| `LIGHTLEAF_REVIEW_AGENT_CONCURRENCY` | `6` | Parallel model calls per review |
| `LIGHTLEAF_REVIEW_AGENT_TIMEOUT` | `600` | Seconds before one reviewer is abandoned |
| `LIGHTLEAF_REVIEW_MAX_INPUT_CHARS` | `800000` | Longer documents are refused, never silently truncated |
| `LIGHTLEAF_REVIEW_PACK` | `papermentor` | Review methodology: `papermentor` or `lightleaf` |
| `LIGHTLEAF_REVIEW_STRICT` | `false` | Strict mode when the request does not say (the dialog has a checkbox) |
| `LIGHTLEAF_REVIEW_DISABLED_AGENTS` | – | Comma list of reviewer ids, e.g. `latex_formatting,venue,abstract_footnote,section_structure` |
| `LIGHTLEAF_REVIEW_SKIP_ANONYMITY` | `false` | papermentor: no comments about anonymous-submission compliance |
| `LIGHTLEAF_REVIEW_SKIP_SOURCE_COMMENTS` | `false` | papermentor: no comments about `%` comments left in the source |
| `LIGHTLEAF_REVIEW_CONTENT_FOCUS` | `false` | papermentor: when pruning in strict mode, prefer content over form |
| `LIGHTLEAF_REVIEW_PROMPTS_DIR` | – | Directory with pack files that replace the built-in ones (same relative paths) |
| `LIGHTLEAF_S2_API_KEY` | – | Semantic Scholar key for the reference check (optional; `SEMANTIC_SCHOLAR_API_KEY` also works) |
| `LIGHTLEAF_S2_MIN_INTERVAL_MS` | `1100` with a key, `3500` without | Minimum time between Semantic Scholar requests |
| `LIGHTLEAF_S2_MAX_RETRIES` / `LIGHTLEAF_S2_TIMEOUT_MS` | `5` / `30000` | Retries on rate limits and outages; per-request timeout |
| `LIGHTLEAF_REFERENCES_MAX_ENTRIES` | `200` | Most cited references checked per run |
| `LIGHTLEAF_REVIEW_FALLBACKS` | `true` | Anthropic only: send `fallbacks: "default"` (turn off for gateways that reject it) |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` | Anthropic API endpoint (e.g. a gateway) |
| `LIGHTLEAF_REVIEW_WORKDIR` | system temp dir | Where checkouts are made |

**Cost.** A review with the `papermentor` pack makes one classification call and one call per reviewer (about
12 for a typical paper). Section reviewers only receive their sections; whole-paper reviewers receive the full
source with their writing guides. The `lightleaf` pack sends the paper once as a cached prefix that all its
reviewers share. Each job's token use is
shown in the Review dialog. Lower `LIGHTLEAF_REVIEW_EFFORT` or disable agents to reduce cost.

## Limits

- LaTeX only for now. Typst projects are refused with a clear message.
- Comments are anchored by exact quotes. If you rewrite a passage while a review is running, its comment is
  skipped (the dialog reports how many could not be placed).
- Not ported from PaperMentor: the Sentence Placement reviewer (it needs full texts of published papers), the
  Acknowledgments/Affiliations editors (one lab's house rules), and role-model paper uploads.
- Quotas on reviews per user are not enforced yet. Use `LIGHTLEAF_REVIEW_MAX_JOBS` and your API key's limits.

## Development

```bash
cargo test -p lightleaf-reviewer        # LaTeX anchoring/insertion, dedup, API request/response handling
```

To try the full flow without API costs, point `ANTHROPIC_BASE_URL` (or `OPENAI_BASE_URL`) at a stub that returns
structured outputs, and `LIGHTLEAF_S2_BASE_URL` at a stub of the Semantic Scholar API.

Code map: `src/review/` (orchestrator, `Pack` and `Reviewer` traits), `src/review/packs/<pack>/` (methodologies),
`packs/<pack>/` (their text files), `src/references/` (reference check), `src/latex/` (parsing, anchoring,
writing comments), `src/llm/` (providers), `src/jobs/` (job kinds, publish), `src/api.rs` (HTTP).
