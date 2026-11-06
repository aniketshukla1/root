# Contributing

Issues and pull requests are welcome. For anything large, open an issue first so the shape can be agreed before the work.

## Run it

Node 22.

```bash
npm install
npm run dev          # the interface in a browser with demo data: http://localhost:1420/?estate=incident
npm run tauri dev    # the desktop app (needs a Rust toolchain and the Tauri 2 prerequisites)
```

`?estate=` also takes `quiet`, `storm` and `empty`.

## Before a pull request

```bash
npx tsc --noEmit -p .
npm test
npm run build && npm run check:budget
cd src-tauri && cargo test --lib && cargo clippy --all-targets -- -D warnings
```

CI runs these and the integration suites. `cargo test --lib` also rewrites `src/contracts.generated.ts` from real backend values, so commit that file if it changes: `tsc` holds it against `src/types.ts`.

## Where things go

- **A new kind of infrastructure** is a backend-only change. See [Adding a service type](docs/REFERENCE.md#adding-a-service-type).
- **A new command** goes in `commands.rs`, `tauri_cmds.rs`, `lib.rs` and `rpc.rs`, with a contract sample if the frontend reads its answer.
- **The frontend's rules** are in [`FRONTEND_ARCHITECTURE.md`](FRONTEND_ARCHITECTURE.md).
- **Demo data** is in `src/fixtures.ts`. It ports backend logic rather than hard-coding answers where it can, so a screen cannot tell a different story from the one beside it.
