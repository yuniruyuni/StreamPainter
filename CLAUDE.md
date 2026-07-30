# StreamPainter development guide

## Product boundary

StreamPainter is a local-only Windows application. The Rust executable owns input, local rendering,
state, and the loopback HTTP/WebSocket service consumed by an OBS Browser Source.

Do not add a hosted web application, login, database, public listener, or cloud deployment unless a
new product decision explicitly changes this boundary.

## Source layout

- `painter/`: Rust/Win32 application, Direct2D renderer, local HTTP/WebSocket server.
- `client/`: transparent OBS Browser Source renderer only.
- `docs/`: architecture, protocol, security, and user setup.

The Rust executable binds only `127.0.0.1`. Keep Host and Origin checks on the WebSocket endpoint.
Slow browser clients must be dropped and recover through a fresh snapshot; never block the Win32 UI
thread on browser I/O.

The JSON protocol is represented in both `painter/src/protocol.rs` and `client/src/protocol.ts`.
Update and test both sides together.

## Commands

Run from the repository root unless noted:

```bash
bun install --frozen-lockfile
bun run check
bun run prepare:painter

cd painter
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings  # run on Windows
cargo build --release --locked
```

Always run `bun run prepare:painter` before Rust checks or builds. It generates the ignored OBS
overlay assets and third-party license page; both are embedded into the executable at compile time.

## Tests

- Client state and geometry: `bun run check:test`
- Rust engine, protocol, local hub, and security checks: `cargo test`
- Full Windows compile/lint/package: `.github/workflows/painter.yml`
