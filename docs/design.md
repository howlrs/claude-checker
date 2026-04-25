# claude-checker design (v0.2 — Rust)

A local kanban dashboard that watches every running Claude Code session in
`~/.claude/` and surfaces them on `localhost:8081` in real time.

KPI: **a session that needs human attention should be visible within 1 second
of it entering that state.**

## 1. Architecture

```
┌─────────────────────────────────────────────┐
│  Browser (localhost:8081)                   │
│  ┌──────────────┬──────────────────────┐   │
│  │ Sessions     │  Tasks Kanban        │   │
│  │ (sidebar)    │  ┌────┬────┬────┐    │   │
│  │ urgency sort │  │TODO│DOING│DONE│   │   │
│  │ urgent banner│  └────┴────┴────┘    │   │
│  └──────────────┴──────────────────────┘   │
│  EventSource('/api/events') ←── SSE         │
└─────────────────────────────────────────────┘
                    ▲
                    │ SSE: session_update / task_update / heartbeat
                    │
┌───────────────────┴─────────────────────────┐
│  Rust binary (axum + tokio)                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │ monitor  │→ │  parser  │→ │  state   │   │
│  │  notify  │  │ FSM + ✅ │  │  Store   │   │
│  └──────────┘  └──────────┘  └──────────┘   │
│        ↑                          ↓         │
│        │                       broadcast    │
└────────┼────────────────────────────────────┘
         │ inotify
   ┌─────┴──────────────────────────┐
   │ ~/.claude/projects/**/*.jsonl  │
   │ ~/.claude/todos/*.json         │
   └────────────────────────────────┘
```

The static assets (`index.html`, `style.css`, `app.js`, `favicon.svg`) are
embedded into the binary via `include_dir!`, so the executable has zero
filesystem dependencies beyond `~/.claude/`.

## 2. Crates (pinned)

| Crate                | Version | Purpose                              |
|----------------------|---------|--------------------------------------|
| `axum`               | 0.8     | HTTP server + SSE                    |
| `tokio`              | 1.52    | Async runtime                        |
| `notify`             | 8.2     | inotify wrapper                      |
| `tower-http`         | 0.6     | Header/middleware utilities          |
| `serde` / `serde_json` | 1.0   | JSON                                 |
| `clap`               | 4.6     | CLI args                             |
| `tracing` / `tracing-subscriber` | 0.1 / 0.3 | Logging                |
| `regex`              | 1.11    | Filename + checklist parsing         |
| `include_dir`        | 0.7     | Compile-time embed of `static/`      |
| `time`               | 0.3     | RFC3339 timestamp parsing            |
| `dirs`               | 5.0     | Cross-platform `~` resolution        |

Toolchain: Rust 1.94 stable, edition 2024.

## 3. Status FSM

Constants (defaults in `parser::Defaults`):

| Constant          | Default | Meaning                                                    |
|-------------------|---------|------------------------------------------------------------|
| `perm_window`     | 30 s    | recent `permission-mode` event → `needs_permission`        |
| `stuck_threshold` | 60 s    | tool_use unresolved this long → `needs_permission`         |
| `stuck_max`       | 30 min  | tool_use older than this is treated as abandoned (ignored) |
| `idle_threshold`  | 6 h     | session is `idle` regardless of other signals              |

The classifier short-circuits in this order:

1. `last_event_ts` is more than `idle_threshold` ago → **`idle`**
2. permission event within `perm_window` → **`needs_permission`**
3. there is a pending `tool_use`:
   - older than `stuck_max` → fall through (ignored)
   - older than `stuck_threshold` → **`needs_permission`**
   - otherwise → **`running`**
4. `has_active_subagent_task` (any TodoWrite item is `in_progress`) → **`running`**
5. `last_stop_ts > 0` and the user hasn't replied → **`waiting_for_user`**
6. activity in the last 5 s without a `stop_reason` yet → **`running`**
7. otherwise → **`idle`**

## 4. Three-tier todo source

For each session the dashboard tries, in order:

1. `~/.claude/todos/<sid>-agent-<sid>.json` — authoritative live state
2. The latest `TodoWrite` tool_use seen in the session JSONL — survives a
   missing todo file
3. `parse_checklist(last_assistant_text)` — last-resort scrape of `✅ / 🔄 /
   ⏳ / 🚧 / ❌ / [x] / [ ]` markers in the assistant's most recent message

The chosen source is sent to the UI as `todos_source` so we can disclose where
the cards came from.

## 5. HTTP API

| Method | Path                  | Description                                |
|--------|-----------------------|--------------------------------------------|
| GET    | `/`                   | `index.html`                               |
| GET    | `/static/{file}`      | embedded asset                             |
| GET    | `/favicon.ico`        | embedded `favicon.svg`                     |
| GET    | `/api/snapshot`       | `{ now, sessions: SessionSummary[] }`      |
| GET    | `/api/session/{id}`   | a single `SessionSummary`                  |
| GET    | `/api/events`         | SSE stream                                 |

SSE event names:

- `session_update` — `data: SessionSummary`
- `task_update`    — `data: { sid, todos }`
- `heartbeat`      — `data: { ts }` (every 15 s)

Reconnect strategy: when the browser detects an SSE error it re-fetches
`/api/snapshot` for a full snapshot — no `Last-Event-ID` plumbing required.

## 6. Filesystem watcher

- Uses `notify::recommended_watcher` (inotify on Linux) to monitor:
  - `~/.claude/projects/` recursively
  - `~/.claude/todos/` non-recursively
- Events are coalesced through a 200 ms per-path debounce so a flurry of
  `IN_MODIFY` events on a busy JSONL collapses into one parse pass.
- JSONL is followed via persisted file offsets in
  `~/.cache/claude-checker/offsets.json` (inode + byte offset). Truncations
  and inode changes reset the offset.
- Todo files are written atomically (rename-replace), so we accept
  `Modify`, `Create`, **and** `Remove` notify events.

## 7. Sorting

Sessions are sorted **urgency-first**, with oldest-first inside the urgent
buckets so the most patient request floats to the top:

1. `needs_permission` (oldest first)
2. `waiting_for_user` (oldest first)
3. `running` (newest first)
4. `idle` (newest first)

The first session in this order is auto-selected on first paint.

## 8. Security

- Binds **only** to `127.0.0.1`. The CLI rejects `--host` values other than
  `127.0.0.1` / `localhost` at startup.
- Strict `Host` header allowlist
  (`localhost`, `localhost:<port>`, `127.0.0.1`, `127.0.0.1:<port>`) — defends
  against DNS rebinding by malicious websites that resolve to `127.0.0.1`.
- CSP: `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
  img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'`.
- All DOM insertions in the frontend use `textContent`. No `innerHTML`,
  no `eval`, no Markdown rendering.
- Path traversal: static asset paths are looked up through `include_dir`
  (compile-time set) and reject any `..` component.

## 9. Tests

- **Unit** (`#[cfg(test)]` in each module): parser FSM, checklist scraper,
  store sort/touch, security host allowlist
- **Integration** (`tests/integration.rs`): boot the full app on an ephemeral
  port with a temp `~/.claude/` and assert:
  - bad `Host` is `403`, good `Host` is `200`, CSP header is present
  - appending JSONL through the watcher produces the expected status
  - atomic-rename of a todo file shows up as kanban cards within ~1 s

Total: **24 tests** at the time of writing.

## 10. Out of scope (v0.2)

- Drag-and-drop reordering or status edits in the UI (would need writes to
  `~/.claude/todos/...`, which we deliberately don't do)
- Authentication tokens (the localhost + Host pair is sufficient)
- Persistent history (the live state is enough; archival would belong in
  SurrealDB or similar)
- WebSocket transport (SSE is one-directional and ample here)
