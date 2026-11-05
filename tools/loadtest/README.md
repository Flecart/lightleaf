# Load test

This simulates many simultaneous editors: users in groups of 5 share a project, type into `main.tex` over the
collaboration WebSocket, and move their cursors. At the same time, 50 HTTP loops call `/api/me`, `/api/projects`,
`/api/projects/<id>` and `/api/projects/<id>/tree`. It reports update fan-out latency (from one editor to the other
editors of the same project) and API latency/throughput.

Requires Node.js 22+ (built-in `WebSocket`) and `frontend/node_modules` (it uses the frontend's `yjs`).

```bash
cargo build --release
export LIGHTLEAF_DATA=/tmp/ll-load LIGHTLEAF_BIND=127.0.0.1:18081 RUST_LOG=lightleaf=warn
rm -rf $LIGHTLEAF_DATA
./target/release/lightleaf user list                          # creates the database
python3 tools/loadtest/seed.py $LIGHTLEAF_DATA/lightleaf.db 1000 /tmp/ll-tokens.json
./target/release/lightleaf &                                   # start the server
BASE=http://127.0.0.1:18081 DURATION=60 node tools/loadtest/load.mjs /tmp/ll-tokens.json
```

`WS=0` or `HTTP=0` disables one half of the load. The load generator is a single Node process. With 1000 clients
it saturates before the server does, so run several generators (with separate token files) to push harder.

## Reference results

22-core Linux machine, release build, 1000 users / 200 projects, 60 s:

| Load | Result |
|---|---|
| WebSockets only (1000 editors, ~500 updates/s in, ~2000/s fanned out) | fan-out p50 **1 ms**, p99 **6 ms** |
| HTTP only (50 concurrent loops) | **~4000 req/s**, p50 11.7 ms, p99 34 ms (client-bound) |
| Both at once | fan-out p50 10 ms / p99 80 ms, ~2200 req/s, 0 errors, ~220 MB RSS |

Compiles are not part of this test. Their cost is set by the TeX engine, `LIGHTLEAF_MAX_COMPILES`, and compile
coalescing.
