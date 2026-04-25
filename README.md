# claude-checker

Local kanban dashboard for [Claude Code](https://www.anthropic.com/claude-code) — a Rust + axum
single-binary that watches `~/.claude/` and serves a real-time view of every Claude session you
have running in WSL/Linux.

> **Not affiliated with Anthropic.** Reads only your local `~/.claude/` files; makes no network
> calls and writes only to a small offset cache under `~/.cache/claude-checker/`.

## Features

- **Left pane** — every session, sorted urgency-first:
  `needs_permission` → `waiting_for_user` → `running` → `idle`
- **Right pane** — `TODO / DOING / DONE` kanban for the selected session
- **Real-time** — SSE + inotify, no polling, no reload
- **Tab/favicon notifications** — `(N) Claude Checker` + red dot when humans are needed,
  so you can leave the tab in the background
- **3-tier todo source** — falls back through `~/.claude/todos/<sid>.json`, then the latest
  `TodoWrite` tool_use in the JSONL, then a plain-text checklist scrape (`✅ / 🔄 / [x] / [ ]`)
- **A11y** — color + icon + text triple-encoding, `j/k/Enter` keyboard navigation
- **Single binary** — static assets are embedded with `include_dir`; no runtime file dependency
  beyond `~/.claude/`

## Install / run

```bash
git clone https://github.com/howlrs/claude-checker.git
cd claude-checker
./start.sh                       # builds + runs (release profile)
# → http://localhost:8081
```

`start.sh` rebuilds when sources are newer than the cached binary, then `exec`s it.

Pinned toolchain:

- Rust **1.94+** (edition 2024)
- `axum 0.8`, `tokio 1.52`, `notify 8.2`, `serde 1`, `tower-http 0.6`, `clap 4.6`

### Supported platforms

Paths are resolved through `dirs::home_dir()` so the same binary works on:

| Platform        | Resolves to                              |
|-----------------|------------------------------------------|
| Linux / WSL2    | `$HOME/.claude/...`                      |
| macOS           | `$HOME/.claude/...`                      |
| Windows         | `%USERPROFILE%\.claude\...`              |

WSL2 specifically: a Claude Code session you start *inside WSL* writes to
the Linux side (`/home/<user>/.claude/`); a session started from native
Windows writes to `C:\Users\<user>\.claude\`. Run claude-checker on the
side where you actually use Claude Code.

### Run from cargo

```bash
cargo run --release -- --port 8081
```

### Environment

- `CC_PORT` — port (default `8081`)
- `CC_HOST` — host (default `127.0.0.1`; refuses anything else)
- `CC_LOG_LEVEL` — `error` / `warn` / `info` / `debug` / `trace`

## Status FSM

| Status              | Trigger                                                                  |
|---------------------|--------------------------------------------------------------------------|
| `needs_permission`  | recent `permission-mode` event, **or** a `tool_use` stuck for >60s       |
| `running`           | unresolved `tool_use`, in-progress TodoWrite, or activity in last 5s     |
| `waiting_for_user`  | last assistant turn ended with `stop_reason` and the user hasn't replied |
| `idle`              | none of the above (covers any session inactive for ≥6h)                  |

Priority: `needs_permission` > `running` > `waiting_for_user` > `idle`.

## Data sources (read-only)

| Path                                                             | Format                                  |
|------------------------------------------------------------------|-----------------------------------------|
| `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`          | append-only event stream                |
| `~/.claude/todos/<session-uuid>-agent-<session-uuid>.json`       | array of `{content, status, activeForm}`|

## Security

- Binds only to `127.0.0.1` (refuses `0.0.0.0` on startup)
- Strict `Host` header allowlist (`localhost` / `127.0.0.1` only) — defends against DNS rebinding
- CSP `default-src 'self'` + `script-src 'self'` — no inline JS, no remote scripts
- All DOM insertion uses `textContent`, never `innerHTML`
- No authentication: the localhost-binding + Host-header pair is the threat model

## Architecture

```
~/.claude/projects/**/*.jsonl ─┐
~/.claude/todos/*.json ────────┼─→ notify (inotify) ─→ parser FSM ─→ Store ─→ broadcast → SSE
                                                                       ↑
                              GET /api/snapshot (resync) ──────────────┘
                              GET /api/session/{id}      (right-pane detail)
```

See [`docs/design.md`](docs/design.md) for the long form.

## Development

```bash
cargo test          # unit + integration tests (24 tests)
cargo clippy
cargo fmt
cargo run -- --log-level debug
```

## License

MIT — see [`LICENSE`](LICENSE).
