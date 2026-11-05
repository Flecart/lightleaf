# Contributing to Lightleaf

Thanks for your interest! Bug reports, ideas and pull requests are all welcome.

## Development setup

You need Rust (stable, edition 2024), Node.js 20+, `git`, and optionally TeX Live and/or Typst for compiling.

```bash
cd frontend && npm install && npm run build && cd ..   # build the UI once (embedded into the binary)
cargo run                                              # backend on :8080
cd frontend && npm run dev                             # hot-reloading UI on :5173 (proxies to :8080)
```

## Before opening a pull request

- `cargo fmt` and `cargo clippy` are clean
- `cargo test` passes
- `cd frontend && npm run build` passes (includes the TypeScript check)
- Keep changes focused; one feature or fix per PR
- For larger changes, open an issue first so we can agree on the approach

## Design principles

Lightleaf aims to stay **small and cheap to run**: one binary, SQLite, plain git repositories on disk, no extra
services. Please keep new dependencies light, and prefer features that cost nothing when they are not in use.

## License and contributor agreement

Lightleaf is licensed under the [AGPL-3.0](LICENSE). By submitting a contribution you agree that:

1. your contribution is licensed under the AGPL-3.0, and
2. you grant the Lightleaf maintainer a perpetual, worldwide, non-exclusive, royalty-free, irrevocable license to use,
   modify and relicense your contribution, including under other (commercial) terms, and
3. you have the right to make the contribution (it is your own work, or you have permission from its owner).

This keeps Lightleaf free for everyone, while allowing it to be offered under a commercial license as well. Please
add a `Signed-off-by:` line to your commits (`git commit -s`) to confirm this.
