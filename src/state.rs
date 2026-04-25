//! In-memory store + tokio broadcast for SSE.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use crate::parser::{now_secs, SessionState, SessionSummary, Todo};

/// One SSE event emitted to all subscribers.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind")]
pub enum Event {
    SessionUpdate(SessionSummary),
    TaskUpdate { sid: String, todos: Vec<Todo> },
    Heartbeat { ts: f64 },
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<RwLock<Inner>>,
    tx: broadcast::Sender<Event>,
}

struct Inner {
    sessions: HashMap<String, SessionState>,
}

impl Store {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        Self {
            inner: Arc::new(RwLock::new(Inner {
                sessions: HashMap::new(),
            })),
            tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Idempotent: get_or_create the session. Updates cwd/file_path if absent.
    pub async fn touch(&self, sid: &str, cwd: &str, file_path: &str) {
        let mut g = self.inner.write().await;
        let s = g
            .sessions
            .entry(sid.to_string())
            .or_insert_with(|| SessionState::new(sid));
        if s.cwd.is_empty() && !cwd.is_empty() {
            s.cwd = cwd.to_string();
        }
        if s.file_path.is_empty() && !file_path.is_empty() {
            s.file_path = file_path.to_string();
        }
    }

    pub async fn with_session_mut<F, R>(&self, sid: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut SessionState) -> R,
    {
        let mut g = self.inner.write().await;
        g.sessions.get_mut(sid).map(f)
    }

    /// Same as `with_session_mut` but creates the session if missing.
    pub async fn mutate_or_create<F, R>(&self, sid: &str, f: F) -> R
    where
        F: FnOnce(&mut SessionState) -> R,
    {
        let mut g = self.inner.write().await;
        let s = g
            .sessions
            .entry(sid.to_string())
            .or_insert_with(|| SessionState::new(sid));
        f(s)
    }

    pub async fn snapshot(&self) -> SnapshotPayload {
        let now = now_secs();
        let g = self.inner.read().await;
        let mut sessions: Vec<SessionSummary> = g
            .sessions
            .values()
            .filter(|s| !s.file_path.is_empty() && s.event_count > 0)
            .map(|s| s.to_summary())
            .collect();
        sessions.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        SnapshotPayload { now, sessions }
    }

    pub async fn session_summary(&self, sid: &str) -> Option<SessionSummary> {
        let g = self.inner.read().await;
        g.sessions.get(sid).map(|s| s.to_summary())
    }

    pub async fn all_summaries(&self) -> Vec<SessionSummary> {
        let g = self.inner.read().await;
        g.sessions.values().map(|s| s.to_summary()).collect()
    }

    pub async fn publish_session(&self, sid: &str) {
        let summary = self.session_summary(sid).await;
        if let Some(summary) = summary {
            let _ = self.tx.send(Event::SessionUpdate(summary));
        }
    }

    pub async fn publish_task(&self, sid: &str) {
        let g = self.inner.read().await;
        if let Some(s) = g.sessions.get(sid) {
            let todos = s.to_summary().todos;
            let _ = self.tx.send(Event::TaskUpdate {
                sid: sid.to_string(),
                todos,
            });
        }
    }

    pub fn publish_heartbeat(&self) {
        let _ = self.tx.send(Event::Heartbeat { ts: now_secs() });
    }
}

#[derive(Serialize)]
pub struct SnapshotPayload {
    pub now: f64,
    pub sessions: Vec<SessionSummary>,
}

/// Same priority as the Python implementation:
///   needs_permission(0) > waiting_for_user(1) > running(2) > idle(3)
/// Within urgent statuses, oldest at top.
fn sort_key(s: &SessionSummary) -> (u8, i64) {
    let rank = match s.status {
        "needs_permission" => 0,
        "waiting_for_user" => 1,
        "running" => 2,
        "idle" => 3,
        _ => 99,
    };
    let ts_ms = (s.last_event_ts * 1000.0) as i64;
    if matches!(s.status, "needs_permission" | "waiting_for_user") {
        (rank, ts_ms) // ascending = oldest first
    } else {
        (rank, -ts_ms) // descending = newest first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn touch_idempotent() {
        let s = Store::new();
        s.touch("sid1", "/x", "/p").await;
        s.touch("sid1", "/y", "/q").await;
        let g = s.inner.read().await;
        let sess = g.sessions.get("sid1").unwrap();
        assert_eq!(sess.cwd, "/x"); // first wins
        assert_eq!(sess.file_path, "/p");
    }

    #[tokio::test]
    async fn snapshot_filters_empty_sessions() {
        let s = Store::new();
        s.touch("sid_empty", "/x", "/no.jsonl").await;
        // event_count is 0 → filtered out
        let snap = s.snapshot().await;
        assert!(snap.sessions.is_empty());
    }

    #[test]
    fn sort_priority() {
        let mk = |status: &'static str, ts: f64| SessionSummary {
            sid: status.into(),
            cwd: "".into(),
            status,
            last_event_ts: ts,
            last_user_input_ts: 0.0,
            last_assistant_ts: 0.0,
            last_stop_ts: 0.0,
            last_stop_reason: "".into(),
            last_tool_name: "".into(),
            pending_tools: vec![],
            first_user_text: "".into(),
            last_assistant_text: "".into(),
            event_count: 1,
            todos: vec![],
            todos_source: "none",
        };
        let mut list = vec![
            mk("idle", 400.0),
            mk("running", 300.0),
            mk("waiting_for_user", 200.0),
            mk("needs_permission", 100.0),
        ];
        list.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        let order: Vec<&str> = list.iter().map(|s| s.status).collect();
        assert_eq!(
            order,
            vec!["needs_permission", "waiting_for_user", "running", "idle"]
        );
    }
}
