//! JSONL event parser, FSM, and assistant-text checklist scraper.
//!
//! Mirrors the Python implementation but is fully synchronous and operates on
//! `serde_json::Value` to stay tolerant of unknown fields (the Claude Code
//! JSONL format is undocumented and changes).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PendingTool {
    pub name: String,
    pub started_at: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
}

#[derive(Clone, Debug, Default)]
pub struct SessionState {
    pub sid: String,
    pub cwd: String,
    pub file_path: String,
    pub pending_tools: HashMap<String, PendingTool>,
    pub last_event_ts: f64,
    pub last_user_input_ts: f64,
    pub last_assistant_ts: f64,
    pub last_stop_ts: f64,
    pub last_stop_reason: String,
    pub last_permission_event_ts: f64,
    pub first_user_text: String,
    pub last_assistant_text: String,
    pub last_tool_name: String,
    pub event_count: u64,
    pub todos: Vec<Todo>,
    pub todos_from_jsonl: Vec<Todo>,
    pub has_active_subagent_task: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub sid: String,
    pub cwd: String,
    pub status: &'static str,
    pub last_event_ts: f64,
    pub last_user_input_ts: f64,
    pub last_assistant_ts: f64,
    pub last_stop_ts: f64,
    pub last_stop_reason: String,
    pub last_tool_name: String,
    pub pending_tools: Vec<PendingToolOut>,
    pub first_user_text: String,
    pub last_assistant_text: String,
    pub event_count: u64,
    pub todos: Vec<Todo>,
    pub todos_source: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingToolOut {
    pub id: String,
    pub name: String,
    pub started_at: f64,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn parse_ts(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339)
            .ok()
            .map(|dt| dt.unix_timestamp() as f64 + dt.nanosecond() as f64 / 1e9),
        _ => None,
    }
}

impl SessionState {
    pub fn new(sid: impl Into<String>) -> Self {
        Self {
            sid: sid.into(),
            ..Default::default()
        }
    }

    pub fn to_summary(&self) -> SessionSummary {
        let status = classify(self, now_secs(), Defaults::default());
        let (todos, todos_source) = if !self.todos.is_empty() {
            (self.todos.clone(), "todo_file")
        } else if !self.todos_from_jsonl.is_empty() {
            (self.todos_from_jsonl.clone(), "jsonl")
        } else {
            let scraped = parse_checklist(&self.last_assistant_text);
            if !scraped.is_empty() {
                (scraped, "checklist")
            } else {
                (Vec::new(), "none")
            }
        };
        let truncate = |s: &str, n: usize| -> String {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() <= n {
                s.to_string()
            } else {
                chars[..n].iter().collect()
            }
        };
        SessionSummary {
            sid: self.sid.clone(),
            cwd: self.cwd.clone(),
            status,
            last_event_ts: self.last_event_ts,
            last_user_input_ts: self.last_user_input_ts,
            last_assistant_ts: self.last_assistant_ts,
            last_stop_ts: self.last_stop_ts,
            last_stop_reason: self.last_stop_reason.clone(),
            last_tool_name: self.last_tool_name.clone(),
            pending_tools: self
                .pending_tools
                .iter()
                .map(|(id, pt)| PendingToolOut {
                    id: id.clone(),
                    name: pt.name.clone(),
                    started_at: pt.started_at,
                })
                .collect(),
            first_user_text: truncate(&self.first_user_text, 200),
            last_assistant_text: truncate(&self.last_assistant_text, 200),
            event_count: self.event_count,
            todos,
            todos_source,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Defaults {
    pub perm_window: f64,
    pub stuck_threshold: f64,
    pub stuck_max: f64,
    pub idle_threshold: f64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            perm_window: 30.0,
            stuck_threshold: 60.0,
            stuck_max: 30.0 * 60.0,
            idle_threshold: 6.0 * 3600.0,
        }
    }
}

pub fn classify(state: &SessionState, now: f64, d: Defaults) -> &'static str {
    let age = if state.last_event_ts > 0.0 {
        now - state.last_event_ts
    } else {
        f64::INFINITY
    };
    if age >= d.idle_threshold {
        return "idle";
    }
    if state.last_permission_event_ts > 0.0
        && now - state.last_permission_event_ts < d.perm_window
    {
        return "needs_permission";
    }
    if !state.pending_tools.is_empty() {
        let oldest = state
            .pending_tools
            .values()
            .map(|p| p.started_at)
            .fold(f64::INFINITY, f64::min);
        let tool_age = now - oldest;
        if tool_age > d.stuck_max {
            // abandoned, fall through
        } else if tool_age > d.stuck_threshold {
            return "needs_permission";
        } else {
            return "running";
        }
    }
    if state.has_active_subagent_task {
        return "running";
    }
    if state.last_stop_ts > 0.0 && state.last_user_input_ts < state.last_stop_ts {
        return "waiting_for_user";
    }
    if state.last_event_ts > 0.0 && age < 5.0 && state.last_stop_ts == 0.0 {
        return "running";
    }
    "idle"
}

pub fn apply_event(state: &mut SessionState, event: &Value) {
    state.event_count += 1;
    let t = event.get("type").and_then(Value::as_str).unwrap_or("");
    let ts = match event.get("timestamp").and_then(parse_ts) {
        Some(v) => v,
        None => return, // events without timestamp don't advance the clock
    };
    state.last_event_ts = ts;

    match t {
        "assistant" => {
            state.last_assistant_ts = ts;
            let msg = event.get("message").cloned().unwrap_or(Value::Null);
            if let Some(content) = msg.get("content").and_then(Value::as_array) {
                for block in content {
                    let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
                    match btype {
                        "tool_use" => {
                            let bid = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if !bid.is_empty() {
                                state.pending_tools.insert(
                                    bid,
                                    PendingTool {
                                        name: name.clone(),
                                        started_at: ts,
                                    },
                                );
                                state.last_tool_name = name.clone();
                            }
                            // TodoWrite fallback capture
                            if name == "TodoWrite" {
                                if let Some(todos) = block
                                    .get("input")
                                    .and_then(|v| v.get("todos"))
                                    .and_then(Value::as_array)
                                {
                                    state.todos_from_jsonl = todos
                                        .iter()
                                        .filter_map(|t| {
                                            let m = t.as_object()?;
                                            Some(Todo {
                                                content: m
                                                    .get("content")
                                                    .and_then(Value::as_str)
                                                    .unwrap_or("")
                                                    .to_string(),
                                                status: m
                                                    .get("status")
                                                    .and_then(Value::as_str)
                                                    .unwrap_or("pending")
                                                    .to_string(),
                                                active_form: m
                                                    .get("activeForm")
                                                    .and_then(Value::as_str)
                                                    .unwrap_or("")
                                                    .to_string(),
                                            })
                                        })
                                        .collect();
                                }
                            }
                        }
                        "text" => {
                            let txt = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if !txt.is_empty() {
                                state.last_assistant_text = txt.to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            if let Some(stop) = msg.get("stop_reason").and_then(Value::as_str) {
                state.last_stop_reason = stop.to_string();
                state.last_stop_ts = ts;
            }
        }
        "user" => {
            let msg = event.get("message").cloned().unwrap_or(Value::Null);
            let content = msg.get("content");
            let mut had_text = false;
            if let Some(arr) = content.and_then(Value::as_array) {
                for block in arr {
                    let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
                    if btype == "tool_result" {
                        if let Some(tid) = block.get("tool_use_id").and_then(Value::as_str) {
                            state.pending_tools.remove(tid);
                        }
                    } else if btype == "text" {
                        had_text = true;
                        if state.first_user_text.is_empty() {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                state.first_user_text = text.to_string();
                            }
                        }
                    }
                }
            } else if let Some(s) = content.and_then(Value::as_str) {
                had_text = true;
                if state.first_user_text.is_empty() {
                    state.first_user_text = s.to_string();
                }
            }
            if had_text {
                state.last_user_input_ts = ts;
            }
        }
        "permission-mode" => {
            state.last_permission_event_ts = ts;
        }
        _ => {}
    }
}

pub fn parse_jsonl_lines<I, S>(lines: I, state: &mut SessionState)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            apply_event(state, &v);
        }
    }
}

// --------------------------------------------------------------------------
// Checklist scraper (3rd-tier todo fallback)
// --------------------------------------------------------------------------

static CHECKLIST_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*[-*]?\s*(?P<marker>✅|✔️|☑️|✓|🔄|⏳|🚧|❌|✗|\[\s*[xX]\s*\]|\[\s*\])\s*(?P<rest>.+?)\s*$",
    )
    .expect("checklist regex compiles")
});

static SQUARE_DONE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\[\s*[xX]\s*\]$").unwrap());
static SQUARE_OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[\s*\]$").unwrap());
static TRAILING_PAREN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s*\([^)]*\)\s*$").unwrap());

const DONE_MARKERS: &[&str] = &["✅", "✔️", "☑️", "✓"];
const IN_PROGRESS_MARKERS: &[&str] = &["🔄", "⏳", "🚧"];
const FAILED_MARKERS: &[&str] = &["❌", "✗"];

pub fn parse_checklist(text: &str) -> Vec<Todo> {
    let mut out = Vec::new();
    if text.is_empty() {
        return out;
    }
    for raw in text.lines() {
        let Some(caps) = CHECKLIST_LINE_RE.captures(raw) else {
            continue;
        };
        let marker = caps.name("marker").unwrap().as_str().trim();
        let rest = caps.name("rest").unwrap().as_str().trim().to_string();
        if rest.is_empty() {
            continue;
        }
        let status = if DONE_MARKERS.contains(&marker) || SQUARE_DONE_RE.is_match(marker) {
            "completed"
        } else if IN_PROGRESS_MARKERS.contains(&marker) {
            "in_progress"
        } else if FAILED_MARKERS.contains(&marker) || SQUARE_OPEN_RE.is_match(marker) {
            "pending"
        } else {
            continue;
        };

        // Comma-split for completed summary lines (✅ A, B, C (3/15))
        let mut items: Vec<String> = vec![rest.clone()];
        if status == "completed" {
            let stripped = TRAILING_PAREN_RE.replace(&rest, "").to_string();
            if stripped.contains(',') && !stripped.contains('。') && !stripped.contains(':') {
                let parts: Vec<String> = stripped
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
                if parts.iter().all(|p| p.chars().count() <= 40) {
                    items = parts;
                }
            }
        }
        for item in items {
            out.push(Todo {
                content: item.clone(),
                status: status.to_string(),
                active_form: item,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(ts: f64) -> String {
        let dt = OffsetDateTime::from_unix_timestamp(ts as i64).unwrap();
        dt.format(&Rfc3339).unwrap()
    }

    fn make() -> SessionState {
        SessionState::new("00000000-0000-0000-0000-000000000001")
    }

    #[test]
    fn running_when_pending_tool_use() {
        let now = now_secs();
        let mut s = make();
        let ev = serde_json::json!({
            "type": "assistant",
            "timestamp": iso(now - 1.0),
            "message": {
                "content": [{"type":"tool_use","id":"t1","name":"Bash"}]
            }
        });
        apply_event(&mut s, &ev);
        assert_eq!(classify(&s, now, Defaults::default()), "running");
    }

    #[test]
    fn waiting_for_user_after_stop_reason() {
        let now = now_secs();
        let mut s = make();
        let user = serde_json::json!({
            "type":"user",
            "timestamp": iso(now - 60.0),
            "message": {"content":[{"type":"text","text":"hi"}]}
        });
        apply_event(&mut s, &user);
        let asst = serde_json::json!({
            "type":"assistant",
            "timestamp": iso(now - 30.0),
            "message": {
                "stop_reason":"end_turn",
                "content":[{"type":"text","text":"hello"}]
            }
        });
        apply_event(&mut s, &asst);
        assert_eq!(classify(&s, now, Defaults::default()), "waiting_for_user");
    }

    #[test]
    fn idle_when_long_inactive() {
        let now = now_secs();
        let mut s = make();
        let ev = serde_json::json!({
            "type":"user",
            "timestamp": iso(now - 7.0 * 3600.0),
            "message": {"content":[{"type":"text","text":"old"}]}
        });
        apply_event(&mut s, &ev);
        assert_eq!(classify(&s, now, Defaults::default()), "idle");
    }

    #[test]
    fn needs_permission_when_tool_stuck_long() {
        let now = now_secs();
        let mut s = make();
        let ev = serde_json::json!({
            "type":"assistant",
            "timestamp": iso(now - 180.0),
            "message": {"content":[{"type":"tool_use","id":"t1","name":"Bash"}]}
        });
        apply_event(&mut s, &ev);
        assert_eq!(classify(&s, now, Defaults::default()), "needs_permission");
    }

    #[test]
    fn needs_permission_after_explicit_event() {
        let now = now_secs();
        let mut s = make();
        let ev = serde_json::json!({"type":"permission-mode","timestamp": iso(now - 5.0)});
        apply_event(&mut s, &ev);
        assert_eq!(classify(&s, now, Defaults::default()), "needs_permission");
    }

    #[test]
    fn priority_perm_beats_running() {
        let now = now_secs();
        let mut s = make();
        let ev = serde_json::json!({
            "type":"assistant",
            "timestamp": iso(now - 1.0),
            "message": {"content":[{"type":"tool_use","id":"t1","name":"X"}]}
        });
        apply_event(&mut s, &ev);
        let perm = serde_json::json!({"type":"permission-mode","timestamp": iso(now - 2.0)});
        apply_event(&mut s, &perm);
        assert_eq!(classify(&s, now, Defaults::default()), "needs_permission");
    }

    #[test]
    fn parse_jsonl_skips_garbage() {
        let mut s = make();
        let lines = vec![
            "".to_string(),
            "not json".to_string(),
            serde_json::json!({"type":"assistant","timestamp": iso(now_secs()),"message":{}})
                .to_string(),
        ];
        parse_jsonl_lines(lines, &mut s);
        assert_eq!(s.event_count, 1);
    }

    // checklist scraper -----------------------------------------------------

    fn by_status(todos: &[Todo]) -> (Vec<&str>, Vec<&str>, Vec<&str>) {
        let mut done = vec![];
        let mut prog = vec![];
        let mut pend = vec![];
        for t in todos {
            match t.status.as_str() {
                "completed" => done.push(t.content.as_str()),
                "in_progress" => prog.push(t.content.as_str()),
                "pending" => pend.push(t.content.as_str()),
                _ => {}
            }
        }
        (pend, prog, done)
    }

    #[test]
    fn money_printer_real_pattern() {
        let text = "INFJ 完了、ENFJ 開始 (02:57:54)。\n\n\
                    - ✅ INFP, ENFP, INFJ (3/15)\n\
                    - 🔄 ENFJ (4/15)\n\
                    - ペース安定 (~13分/タイプ)、完了見込み 5:00〜5:15\n";
        let todos = parse_checklist(text);
        let (pend, prog, done) = by_status(&todos);
        assert_eq!(done, vec!["INFP", "ENFP", "INFJ"]);
        assert!(prog.iter().any(|c| c.contains("ENFJ")));
        assert!(pend.is_empty());
    }

    #[test]
    fn brackets_pattern() {
        let text = "- [x] task A\n- [ ] task B\n- [X] task C\n";
        let todos = parse_checklist(text);
        let (pend, prog, done) = by_status(&todos);
        assert_eq!(done, vec!["task A", "task C"]);
        assert_eq!(pend, vec!["task B"]);
        assert!(prog.is_empty());
    }

    #[test]
    fn empty_text_returns_empty() {
        assert!(parse_checklist("").is_empty());
    }

    #[test]
    fn no_markers_returns_empty() {
        assert!(parse_checklist("Just a paragraph with no checklist.").is_empty());
    }

    #[test]
    fn in_progress_emojis() {
        let text = "- 🔄 building\n- ⏳ waiting for review\n- 🚧 deploying\n";
        let todos = parse_checklist(text);
        assert!(todos.iter().all(|t| t.status == "in_progress"));
        let names: Vec<&str> = todos.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(names, vec!["building", "waiting for review", "deploying"]);
    }

    #[test]
    fn failed_marker_stays_pending() {
        let text = "- ❌ broken thing\n- ✅ fixed thing\n";
        let todos = parse_checklist(text);
        let (pend, _prog, done) = by_status(&todos);
        assert_eq!(pend, vec!["broken thing"]);
        assert_eq!(done, vec!["fixed thing"]);
    }

    #[test]
    fn completed_with_sentence_does_not_split() {
        let text = "- ✅ INFP, ENFP のテスト完了。次は INFJ。\n";
        let todos = parse_checklist(text);
        assert_eq!(todos.len(), 1);
        assert!(todos[0].content.contains("INFP"));
    }

    #[test]
    fn summary_paren_count_stripped() {
        let text = "- ✅ A, B, C (3/15)\n";
        let todos = parse_checklist(text);
        let names: Vec<&str> = todos.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    #[test]
    fn in_progress_not_split() {
        let text = "- 🔄 step1, step2 (running)\n";
        let todos = parse_checklist(text);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].status, "in_progress");
    }
}
