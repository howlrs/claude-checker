//! Filesystem monitor for ~/.claude/{projects,todos}.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
// MetadataExt for inode is Unix-only. On Windows we fall back to file
// length to detect truncation (Claude Code never rotates JSONL anyway).
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

use crate::parser::{apply_event, Todo};
use crate::state::Store;

static UUID_JSONL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$")
        .unwrap()
});
// `regex` crate doesn't support backreferences, so we capture both halves
// and verify equality in code below.
static TODO_FILE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([0-9a-f-]{36})-agent-([0-9a-f-]{36})\.json$").unwrap()
});

#[derive(Clone, Debug)]
pub struct MonitorConfig {
    pub projects_root: PathBuf,
    pub todos_root: PathBuf,
    pub offsets_path: PathBuf,
    pub debounce: Duration,
}

impl MonitorConfig {
    /// Resolve paths relative to the user's home directory.
    ///
    /// Cross-platform: `dirs::home_dir()` returns `$HOME` on Linux/macOS
    /// and `%USERPROFILE%` on Windows. WSL2 reads its Linux `$HOME`, so a
    /// session started from WSL ends up under `/home/<user>/.claude/` and
    /// a session started from native Windows under `C:\Users\<user>\.claude\`,
    /// which is exactly where Claude Code writes them.
    pub fn from_home() -> anyhow::Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory ($HOME unset)"))?;
        let cache = dirs::cache_dir()
            .unwrap_or_else(|| home.join(".cache"))
            .join("claude-checker");
        Ok(Self {
            projects_root: home.join(".claude").join("projects"),
            todos_root: home.join(".claude").join("todos"),
            offsets_path: cache.join("offsets.json"),
            debounce: Duration::from_millis(200),
        })
    }
}

pub fn decode_cwd(encoded: &str) -> String {
    if let Some(rest) = encoded.strip_prefix('-') {
        format!("/{}", rest.replace('-', "/"))
    } else {
        encoded.to_string()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct OffsetMap {
    #[serde(flatten)]
    map: HashMap<String, OffsetEntry>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct OffsetEntry {
    offset: u64,
    inode: u64,
}

#[derive(Clone)]
pub struct Monitor {
    pub cfg: MonitorConfig,
    pub store: Store,
    offsets: Arc<Mutex<OffsetMap>>,
}

impl Monitor {
    pub fn new(store: Store, cfg: MonitorConfig) -> Self {
        let offsets = if cfg.offsets_path.exists() {
            std::fs::read_to_string(&cfg.offsets_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        } else {
            OffsetMap::default()
        };
        Self {
            cfg,
            store,
            offsets: Arc::new(Mutex::new(offsets)),
        }
    }

    pub async fn initial_scan(&self) -> Result<()> {
        if self.cfg.projects_root.is_dir() {
            for entry in fs::read_dir(&self.cfg.projects_root)? {
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dirname = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let cwd = decode_cwd(&dirname);
                if let Ok(rd) = fs::read_dir(&path) {
                    for f in rd.flatten() {
                        let p = f.path();
                        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                            if UUID_JSONL.is_match(name) {
                                let sid = name.trim_end_matches(".jsonl").to_string();
                                self.store.touch(&sid, &cwd, p.to_str().unwrap_or("")).await;
                                // Initial scan: ignore the persisted offset
                                // and re-read the whole file so every
                                // session is materialized in memory.
                                let _ = self.read_jsonl(&p, &sid, false, true).await;
                            }
                        }
                    }
                }
            }
        }
        if self.cfg.todos_root.is_dir() {
            if let Ok(rd) = fs::read_dir(&self.cfg.todos_root) {
                for f in rd.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("json") {
                        let _ = self.read_todo(&p, false).await;
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn flush_offsets(&self) -> Result<()> {
        let g = self.offsets.lock().await;
        if let Some(parent) = self.cfg.offsets_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let s = serde_json::to_string(&*g)?;
        fs::write(&self.cfg.offsets_path, s)
            .with_context(|| format!("write offsets {:?}", self.cfg.offsets_path))?;
        Ok(())
    }

    /// Spawn the watcher + debouncer + tick loop. Returns immediately.
    pub fn spawn(self: Arc<Self>) -> Result<RecommendedWatcher> {
        let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();

        // 1) notify watcher → just forward paths
        let tx_for_notify = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(ev) = res else { return };
            if !matches!(
                ev.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                // also accept untyped Any
            }
            for path in ev.paths {
                let _ = tx_for_notify.send(path);
            }
        })?;
        if self.cfg.projects_root.is_dir() {
            watcher.watch(&self.cfg.projects_root, RecursiveMode::Recursive)?;
        }
        if self.cfg.todos_root.is_dir() {
            watcher.watch(&self.cfg.todos_root, RecursiveMode::NonRecursive)?;
        }

        // 2) debouncer task
        let monitor = self.clone();
        tokio::spawn(async move {
            // (path → last seen tick)
            let mut pending: HashMap<PathBuf, tokio::time::Instant> = HashMap::new();
            let debounce = monitor.cfg.debounce;
            loop {
                tokio::select! {
                    Some(path) = rx.recv() => {
                        pending.insert(path, tokio::time::Instant::now() + debounce);
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        let now = tokio::time::Instant::now();
                        let due: Vec<PathBuf> = pending
                            .iter()
                            .filter(|(_, t)| **t <= now)
                            .map(|(p, _)| p.clone())
                            .collect();
                        for p in due {
                            pending.remove(&p);
                            if let Err(e) = monitor.process_path(&p).await {
                                tracing::warn!("process_path {:?}: {e:?}", p);
                            }
                        }
                    }
                }
            }
        });

        // 3) tick loop: re-classify & republish on status changes
        let monitor = self.clone();
        tokio::spawn(async move {
            let mut last_status: HashMap<String, &'static str> = HashMap::new();
            let mut last_heartbeat = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let summaries = monitor.store.all_summaries().await;
                for s in summaries {
                    let cur = s.status;
                    let prev = last_status.get(&s.sid);
                    if prev != Some(&cur) {
                        last_status.insert(s.sid.clone(), cur);
                        monitor.store.publish_session(&s.sid).await;
                    }
                }
                if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                    monitor.store.publish_heartbeat();
                    last_heartbeat = tokio::time::Instant::now();
                }
                let _ = monitor.flush_offsets().await;
            }
        });

        Ok(watcher)
    }

    async fn process_path(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let parent = path.parent().unwrap_or(Path::new(""));
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext == "jsonl" && parent.starts_with(&self.cfg.projects_root) {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if UUID_JSONL.is_match(name) {
                    let sid = name.trim_end_matches(".jsonl").to_string();
                    let cwd = parent
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(decode_cwd)
                        .unwrap_or_default();
                    self.store.touch(&sid, &cwd, path.to_str().unwrap_or("")).await;
                    self.read_jsonl(path, &sid, true, false).await?;
                }
            }
        } else if ext == "json" && parent == self.cfg.todos_root {
            self.read_todo(path, true).await?;
        }
        Ok(())
    }

    async fn read_jsonl(
        &self,
        path: &Path,
        sid: &str,
        publish: bool,
        force_full: bool,
    ) -> Result<()> {
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        #[cfg(unix)]
        let inode = meta.ino();
        #[cfg(not(unix))]
        let inode: u64 = 0; // best-effort; Windows uses size-only invariants
        let size = meta.len();
        let key = path.to_string_lossy().to_string();
        let mut g = self.offsets.lock().await;
        let entry = g.map.entry(key.clone()).or_default();
        if force_full || entry.inode != inode || entry.offset > size {
            entry.offset = 0;
        }
        let start = entry.offset;
        drop(g);

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let last_nl = text.rfind('\n');
        let Some(end) = last_nl else {
            return Ok(());
        };
        let complete = &text[..=end];
        let consumed = complete.len() as u64;
        let lines: Vec<&str> = complete.split('\n').collect();

        // Parse without holding the offsets lock.
        self.store
            .mutate_or_create(sid, |s| {
                for line in lines {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        apply_event(s, &v);
                    }
                }
            })
            .await;

        // Update offset.
        let mut g = self.offsets.lock().await;
        let entry = g.map.entry(key).or_default();
        entry.offset = start + consumed;
        entry.inode = inode;
        drop(g);

        if publish {
            self.store.publish_session(sid).await;
        }
        Ok(())
    }

    async fn read_todo(&self, path: &Path, publish: bool) -> Result<()> {
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => return Ok(()),
        };
        let Some(caps) = TODO_FILE.captures(&name) else {
            return Ok(());
        };
        let sid = caps.get(1).unwrap().as_str();
        let sid2 = caps.get(2).unwrap().as_str();
        if sid != sid2 {
            return Ok(());
        }
        let sid = sid.to_string();
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let arr: Vec<Value> = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let mut clean: Vec<Todo> = Vec::with_capacity(arr.len());
        for item in arr {
            let m = match item.as_object() {
                Some(m) => m,
                None => continue,
            };
            clean.push(Todo {
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
            });
        }
        let active = clean.iter().any(|t| t.status == "in_progress");
        self.store
            .mutate_or_create(&sid, |s| {
                s.todos = clean.clone();
                s.has_active_subagent_task = active;
            })
            .await;
        if publish {
            self.store.publish_task(&sid).await;
            self.store.publish_session(&sid).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_cwd_basic() {
        assert_eq!(decode_cwd("-home-user-projects"), "/home/user/projects");
        assert_eq!(decode_cwd("-Users-alice-code"), "/Users/alice/code");
        assert_eq!(decode_cwd("plain"), "plain");
    }
}
