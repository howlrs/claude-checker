//! Integration tests for claude-checker.
//!
//! Spawns the axum app on an ephemeral port with monitor roots pointed at a
//! tempdir so we don't read the user's real ~/.claude/.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use claude_checker::{
    monitor::{Monitor, MonitorConfig},
    server::{router, AppState},
    state::Store,
};
use serde_json::Value;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::time::sleep;

const SID: &str = "00000000-0000-0000-0000-0000000000aa";
const ENCODED: &str = "-tmp-fake-project";

async fn boot(
    cfg: MonitorConfig,
) -> (Arc<Monitor>, SocketAddr, tokio::task::JoinHandle<()>) {
    let store = Store::new();
    let monitor = Arc::new(Monitor::new(store.clone(), cfg));
    monitor.initial_scan().await.unwrap();
    let _watcher = monitor.clone().spawn().unwrap();
    // keep watcher alive for the duration of the test by leaking it; in real
    // life it would live in main()
    Box::leak(Box::new(_watcher));

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    let bound = listener.local_addr().unwrap();
    let app = router(AppState {
        store,
        port: bound.port(),
    });
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (monitor, bound, handle)
}

fn make_cfg(tmp: &Path) -> MonitorConfig {
    MonitorConfig {
        projects_root: tmp.join("projects"),
        todos_root: tmp.join("todos"),
        offsets_path: tmp.join("offsets.json"),
        debounce: Duration::from_millis(50),
    }
}

async fn write_jsonl(path: &Path, lines: &[Value]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for v in lines {
        writeln!(f, "{v}").unwrap();
    }
    f.flush().unwrap();
}

#[tokio::test]
async fn snapshot_and_host_header() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects").join(ENCODED)).unwrap();
    std::fs::create_dir_all(dir.path().join("todos")).unwrap();

    let cfg = make_cfg(dir.path());
    let (_m, addr, _h) = boot(cfg).await;
    sleep(Duration::from_millis(200)).await;

    // Bad host → 403
    let client = reqwest::Client::new();
    let bad = client
        .get(format!("http://{addr}/api/snapshot"))
        .header(reqwest::header::HOST, "evil.example:8081")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::FORBIDDEN);

    // Good host → 200, JSON shape
    let ok = client
        .get(format!("http://{addr}/api/snapshot"))
        .header(reqwest::header::HOST, format!("127.0.0.1:{}", addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert!(
        ok.headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .is_some(),
        "CSP header must be present"
    );
    let body: Value = ok.json().await.unwrap();
    assert!(body.get("sessions").is_some());
}

#[tokio::test]
async fn jsonl_picked_up_by_watcher() {
    let dir = tempdir().unwrap();
    let proj = dir.path().join("projects").join(ENCODED);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(dir.path().join("todos")).unwrap();
    let sess = proj.join(format!("{SID}.jsonl"));

    let cfg = make_cfg(dir.path());
    let (_m, addr, _h) = boot(cfg).await;
    sleep(Duration::from_millis(150)).await;

    // craft a `waiting_for_user` session: user → assistant with stop_reason
    let now = claude_checker::parser::now_secs();
    let to_iso = |ts: f64| {
        let dt = time::OffsetDateTime::from_unix_timestamp(ts as i64).unwrap();
        dt.format(&time::format_description::well_known::Rfc3339).unwrap()
    };
    write_jsonl(
        &sess,
        &[
            serde_json::json!({
                "type":"user",
                "timestamp": to_iso(now - 60.0),
                "message": {"content":[{"type":"text","text":"ping"}]}
            }),
            serde_json::json!({
                "type":"assistant",
                "timestamp": to_iso(now - 30.0),
                "message": {
                    "stop_reason":"end_turn",
                    "content":[{"type":"text","text":"pong"}]
                }
            }),
        ],
    )
    .await;

    // wait for the watcher (debounce 50ms) + tick
    let mut got = false;
    let client = reqwest::Client::new();
    for _ in 0..40 {
        sleep(Duration::from_millis(150)).await;
        let r = client
            .get(format!("http://{addr}/api/snapshot"))
            .header(reqwest::header::HOST, format!("127.0.0.1:{}", addr.port()))
            .send()
            .await
            .unwrap();
        let body: Value = r.json().await.unwrap();
        let sessions = body.get("sessions").and_then(|v| v.as_array()).unwrap();
        if let Some(s) = sessions.iter().find(|s| s.get("sid").and_then(Value::as_str) == Some(SID)) {
            assert_eq!(s.get("status").and_then(Value::as_str), Some("waiting_for_user"));
            got = true;
            break;
        }
    }
    assert!(got, "session not picked up by watcher in time");
}

#[tokio::test]
async fn todo_file_updates_kanban() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    std::fs::create_dir_all(dir.path().join("todos")).unwrap();

    let cfg = make_cfg(dir.path());
    let (_m, addr, _h) = boot(cfg).await;
    sleep(Duration::from_millis(150)).await;

    // We need an event_count > 0 so the session is visible: write a tiny jsonl too.
    let proj = dir.path().join("projects").join(ENCODED);
    std::fs::create_dir_all(&proj).unwrap();
    let sess = proj.join(format!("{SID}.jsonl"));
    let now = claude_checker::parser::now_secs();
    let to_iso = |ts: f64| {
        let dt = time::OffsetDateTime::from_unix_timestamp(ts as i64).unwrap();
        dt.format(&time::format_description::well_known::Rfc3339).unwrap()
    };
    write_jsonl(
        &sess,
        &[serde_json::json!({
            "type":"user",
            "timestamp": to_iso(now - 5.0),
            "message": {"content":[{"type":"text","text":"hi"}]}
        })],
    )
    .await;

    // atomic-rename style write of the todo file
    let todo_tmp = dir.path().join("todos").join("tmp.json");
    std::fs::write(
        &todo_tmp,
        r#"[{"content":"a","status":"pending","activeForm":"ing a"},{"content":"b","status":"in_progress","activeForm":"ing b"}]"#,
    )
    .unwrap();
    let final_path = dir
        .path()
        .join("todos")
        .join(format!("{SID}-agent-{SID}.json"));
    std::fs::rename(&todo_tmp, &final_path).unwrap();

    let client = reqwest::Client::new();
    let mut got = false;
    for _ in 0..40 {
        sleep(Duration::from_millis(150)).await;
        let r = client
            .get(format!("http://{addr}/api/snapshot"))
            .header(reqwest::header::HOST, format!("127.0.0.1:{}", addr.port()))
            .send()
            .await
            .unwrap();
        let body: Value = r.json().await.unwrap();
        let sessions = body["sessions"].as_array().unwrap();
        if let Some(s) = sessions.iter().find(|s| s["sid"] == SID) {
            if let Some(todos) = s.get("todos").and_then(|v| v.as_array()) {
                if todos.len() == 2 {
                    assert_eq!(s["todos_source"], "todo_file");
                    got = true;
                    break;
                }
            }
        }
    }
    assert!(got, "todo file did not propagate in time");
}
