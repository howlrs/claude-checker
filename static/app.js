// ESM entrypoint. Loaded via <script type="module"> against the import-map
// in index.html. We use `htm` for JSX-like syntax inside plain template
// literals so we don't need Babel-standalone or any bundler.
//
// React 19 (ESM, from esm.sh) is bridged into `window.React` / `window.ReactDOM`
// inside index.html so the Ant Design 5 UMD bundle (loaded from jsDelivr) can
// resolve them as globals — guaranteeing a single React instance across both.

import React, {
  useState,
  useEffect,
  useMemo,
  useCallback,
  useRef,
} from "react";
import { createRoot } from "react-dom/client";
import htm from "htm";

// Wait until antd UMD has finished loading and attached to window.antd.
async function waitForAntd() {
  if (window.antd) return window.antd;
  await new Promise((resolve) => {
    const tick = () => {
      if (window.antd) resolve();
      else setTimeout(tick, 25);
    };
    tick();
  });
  return window.antd;
}

const antd = await waitForAntd();
const {
  ConfigProvider,
  Layout,
  List,
  Tag,
  Badge,
  Alert,
  Empty,
  Tooltip,
  Card,
  Row,
  Col,
  Typography,
  Button,
  Avatar,
  Flex,
  theme,
} = antd;

const html = htm.bind(React.createElement);
const { Sider, Content, Header } = Layout;
const { Text, Title } = Typography;

// ---------------------------------------------------------------------------
// Constants & helpers
// ---------------------------------------------------------------------------

const STATUS_LABEL = {
  needs_permission: "許可待ち",
  waiting_for_user: "応答待ち",
  running: "実行中",
  idle: "idle",
};
const STATUS_RANK = {
  needs_permission: 0,
  waiting_for_user: 1,
  running: 2,
  idle: 3,
};
const STATUS_COLOR = {
  needs_permission: "#e53935",
  waiting_for_user: "#fbc02d",
  running: "#1e88e5",
  idle: "#6b7280",
};
const STATUS_TAG_COLOR = {
  needs_permission: "error",
  waiting_for_user: "warning",
  running: "processing",
  idle: "default",
};
const STATUS_GLYPH = {
  needs_permission: "!",
  waiting_for_user: "?",
  running: "▶",
  idle: "·",
};

function formatAge(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 60) return `${Math.floor(seconds)}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

function shortCwd(cwd) {
  if (!cwd) return "";
  const parts = cwd.split("/").filter(Boolean);
  if (parts.length <= 2) return cwd;
  return ".../" + parts.slice(-2).join("/");
}

function shortSid(sid) {
  return sid.slice(0, 8);
}

function makeFavicon(hasUrgent, hasPerms) {
  const c = document.createElement("canvas");
  c.width = 32;
  c.height = 32;
  const g = c.getContext("2d");
  g.fillStyle = "#0f1115";
  g.fillRect(0, 0, 32, 32);
  g.fillStyle = "#5ac8fa";
  g.font = "bold 18px sans-serif";
  g.textBaseline = "middle";
  g.textAlign = "center";
  g.fillText("C", 16, 17);
  if (hasUrgent) {
    g.beginPath();
    g.arc(24, 8, 7, 0, Math.PI * 2);
    g.fillStyle = hasPerms ? "#e53935" : "#fbc02d";
    g.fill();
  }
  return c.toDataURL("image/png");
}

function sortSessions(sessions) {
  const list = [...sessions];
  list.sort((a, b) => {
    const ra = STATUS_RANK[a.status] ?? 99;
    const rb = STATUS_RANK[b.status] ?? 99;
    if (ra !== rb) return ra - rb;
    if (a.status === "needs_permission" || a.status === "waiting_for_user") {
      return (a.last_event_ts || 0) - (b.last_event_ts || 0);
    }
    return (b.last_event_ts || 0) - (a.last_event_ts || 0);
  });
  return list;
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

function useServerClock() {
  const [serverNow, setServerNow] = useState(0);
  const clientAtSync = useRef(Date.now() / 1000);

  const sync = useCallback((srvNow) => {
    setServerNow(srvNow);
    clientAtSync.current = Date.now() / 1000;
  }, []);

  const nowAdjusted = useCallback(
    () => serverNow + (Date.now() / 1000 - clientAtSync.current),
    [serverNow],
  );

  return { sync, nowAdjusted };
}

function useTick(intervalMs = 1000) {
  const [, setT] = useState(0);
  useEffect(() => {
    const h = setInterval(() => setT((x) => x + 1), intervalMs);
    return () => clearInterval(h);
  }, [intervalMs]);
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

function StatusDot({ status }) {
  const color = STATUS_COLOR[status] || "#6b7280";
  const glyph = STATUS_GLYPH[status] || "·";
  const pulse = status === "needs_permission";
  return html`
    <${Avatar}
      size=${20}
      style=${{
        backgroundColor: color,
        color: status === "waiting_for_user" ? "#1f2937" : "#fff",
        fontWeight: 700,
        fontSize: 12,
        animation: pulse ? "cc-pulse 1.6s infinite" : undefined,
        flexShrink: 0,
      }}
    >${glyph}<//>
  `;
}

function StatusTag({ status }) {
  return html`
    <${Tag} color=${STATUS_TAG_COLOR[status]} style=${{ marginInlineEnd: 0 }}>
      ${STATUS_LABEL[status] || status}
    <//>
  `;
}

function SessionRow({ session, selected, ageSeconds, onClick }) {
  const text =
    session.first_user_text ||
    session.last_assistant_text ||
    `session ${shortSid(session.sid)}`;
  return html`
    <${List.Item}
      onClick=${onClick}
      style=${{
        cursor: "pointer",
        padding: "10px 14px",
        background: selected ? "rgba(90, 200, 250, 0.08)" : undefined,
        borderInlineStart: selected
          ? "3px solid #5ac8fa"
          : "3px solid transparent",
        transition: "background 80ms ease",
      }}
    >
      <${Flex} gap=${10} align="flex-start" style=${{ width: "100%" }}>
        <${StatusDot} status=${session.status} />
        <${Flex}
          vertical
          gap=${4}
          style=${{ flex: 1, minWidth: 0, overflow: "hidden" }}
        >
          <${Text}
            strong
            ellipsis
            style=${{ fontSize: 13, maxWidth: "100%" }}
          >
            ${text.replace(/\s+/g, " ").trim()}
          <//>
          <${Flex} gap=${6} align="center" wrap="nowrap" style=${{ minWidth: 0 }}>
            <${StatusTag} status=${session.status} />
            <${Text}
              type="secondary"
              ellipsis
              style=${{ fontSize: 11, flex: 1, minWidth: 0 }}
            >
              ${shortCwd(session.cwd)}
            <//>
          <//>
        <//>
        <${Tooltip} title="最終イベントからの経過時間">
          <${Text}
            type="secondary"
            style=${{
              fontSize: 11,
              fontVariantNumeric: "tabular-nums",
              flexShrink: 0,
            }}
          >
            ${formatAge(ageSeconds)}
          <//>
        <//>
      <//>
    <//>
  `;
}

function UrgentBanner({ count, onJump }) {
  if (count === 0) return null;
  return html`
    <${Alert}
      type="error"
      banner
      showIcon
      message=${html`
        <${Flex} justify="space-between" align="center">
          <${Text} strong style=${{ color: "#fff" }}>
            人間待ち ${count} 件
          <//>
          <${Button} size="small" onClick=${onJump} ghost>先頭へ<//>
        <//>
      `}
      style=${{
        margin: 8,
        borderRadius: 6,
        background: "#e53935",
        border: "none",
        animation: "cc-pulse 2s infinite",
      }}
    />
  `;
}

function TaskCard({ todo, lane }) {
  const styleByLane = {
    pending: { borderInlineStart: "3px solid #94a3b8" },
    in_progress: { borderInlineStart: "3px solid #1e88e5" },
    completed: {
      borderInlineStart: "3px solid #22c55e",
      opacity: 0.65,
      textDecoration: "line-through",
    },
  };
  const text =
    lane === "in_progress"
      ? todo.activeForm || todo.content
      : todo.content || todo.activeForm || "";
  return html`
    <${Card}
      size="small"
      styles=${{ body: { padding: "8px 10px" } }}
      style=${{ marginBottom: 6, ...styleByLane[lane] }}
    >
      <${Text}
        style=${{
          fontSize: 12.5,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
        }}
      >${text}<//>
    <//>
  `;
}

function Lane({ title, todos, lane, count }) {
  return html`
    <${Card}
      size="small"
      title=${html`
        <${Flex} justify="space-between" align="center">
          <${Text} style=${{ fontSize: 12, letterSpacing: "0.06em" }}>${title}<//>
          <${Badge}
            count=${count}
            showZero
            color="#1f2531"
            style=${{ color: "#e8ecf2" }}
          />
        <//>
      `}
      styles=${{
        header: { padding: "6px 10px", minHeight: 32 },
        body: {
          padding: 8,
          height: "calc(100vh - 44px - 24px - 96px - 38px)",
          overflowY: "auto",
        },
      }}
    >
      ${todos.length === 0
        ? html`<${Empty}
            imageStyle=${{ height: 28 }}
            description=${html`<${Text}
              type="secondary"
              style=${{ fontSize: 11 }}
            >(なし)<//>`}
          />`
        : todos.map(
            (t, i) => html`<${TaskCard} key=${`${i}-${t.content}`} todo=${t} lane=${lane} />`,
          )}
    <//>
  `;
}

function SourceHint({ source, hasTodos }) {
  if (!hasTodos) {
    return html`
      <${Alert}
        type="warning"
        showIcon
        style=${{ marginTop: 8 }}
        message="このセッションは TodoWrite を呼んでおらず、進捗チェックリストも検出できませんでした。"
      />
    `;
  }
  if (source === "jsonl") {
    return html`
      <${Text}
        type="secondary"
        style=${{ fontSize: 11, display: "block", marginTop: 4 }}
      >
        (JSONL の最新 TodoWrite から復元)
      <//>
    `;
  }
  if (source === "checklist") {
    return html`
      <${Text}
        style=${{
          fontSize: 11,
          color: "#5ac8fa",
          display: "block",
          marginTop: 4,
        }}
      >
        (assistant メッセージのチェックリストから自動抽出)
      <//>
    `;
  }
  return null;
}

function BoardHeader({ session, ageSeconds }) {
  const main =
    session.first_user_text ||
    session.last_assistant_text ||
    `session ${shortSid(session.sid)}`;
  const tool = session.last_tool_name ? ` · tool: ${session.last_tool_name}` : "";
  return html`
    <div
      style=${{
        padding: "12px 16px",
        background: "#161a22",
        borderBottom: "1px solid #2a3140",
      }}
    >
      <${Flex} gap=${8} align="center" wrap="wrap">
        <${StatusTag} status=${session.status} />
        <${Title} level=${5} style=${{ margin: 0 }}>
          ${main.replace(/\s+/g, " ").slice(0, 120)}
        <//>
      <//>
      <${Text} type="secondary" style=${{ fontSize: 11 }}>
        ${shortCwd(session.cwd)} · ${shortSid(session.sid)} · 最終 ${formatAge(ageSeconds)} 前${tool}
      <//>
      <${SourceHint}
        source=${session.todos_source}
        hasTodos=${(session.todos || []).length > 0}
      />
    </div>
  `;
}

function Board({ session, nowAdjusted }) {
  if (!session) {
    return html`
      <${Flex}
        align="center"
        justify="center"
        style=${{ height: "calc(100vh - 44px)" }}
      >
        <${Empty} description="左から選択してください。緊急セッションがあれば自動選択中。" />
      <//>
    `;
  }
  const todos = session.todos || [];
  const buckets = { pending: [], in_progress: [], completed: [] };
  for (const t of todos) {
    const k = buckets[t.status] ? t.status : "pending";
    buckets[k].push(t);
  }
  const ageSeconds = nowAdjusted - (session.last_event_ts || 0);
  return html`
    <${React.Fragment}>
      <${BoardHeader} session=${session} ageSeconds=${ageSeconds} />
      <div style=${{ padding: 12 }}>
        <${Row} gutter=${12}>
          <${Col} span=${8}>
            <${Lane}
              title="TODO"
              lane="pending"
              todos=${buckets.pending}
              count=${buckets.pending.length}
            />
          <//>
          <${Col} span=${8}>
            <${Lane}
              title="DOING"
              lane="in_progress"
              todos=${buckets.in_progress}
              count=${buckets.in_progress.length}
            />
          <//>
          <${Col} span=${8}>
            <${Lane}
              title="DONE"
              lane="completed"
              todos=${buckets.completed}
              count=${buckets.completed.length}
            />
          <//>
        <//>
      </div>
    <//>
  `;
}

function App() {
  const [sessions, setSessions] = useState(new Map());
  const [selectedSid, setSelectedSid] = useState(null);
  const [conn, setConn] = useState({ text: "connecting…", level: "default" });
  const { sync, nowAdjusted } = useServerClock();
  const lastUrgentRef = useRef(-1);
  useTick(1000);

  const sortedList = useMemo(
    () => sortSessions(Array.from(sessions.values())),
    [sessions],
  );

  const urgentCount = useMemo(
    () =>
      sortedList.filter(
        (s) =>
          s.status === "needs_permission" || s.status === "waiting_for_user",
      ).length,
    [sortedList],
  );
  const permsCount = useMemo(
    () => sortedList.filter((s) => s.status === "needs_permission").length,
    [sortedList],
  );

  useEffect(() => {
    document.title =
      urgentCount > 0 ? `(${urgentCount}) Claude Checker` : "Claude Checker";
    if (lastUrgentRef.current !== urgentCount) {
      lastUrgentRef.current = urgentCount;
      const link = document.querySelector("link[rel='icon']");
      if (link) link.href = makeFavicon(urgentCount > 0, permsCount > 0);
    }
  }, [urgentCount, permsCount]);

  useEffect(() => {
    if (!selectedSid && sortedList.length > 0) {
      setSelectedSid(sortedList[0].sid);
    }
  }, [selectedSid, sortedList]);

  const loadSnapshot = useCallback(async () => {
    try {
      const res = await fetch("/api/snapshot", { credentials: "same-origin" });
      if (!res.ok) throw new Error(`snapshot ${res.status}`);
      const data = await res.json();
      sync(data.now || Date.now() / 1000);
      const m = new Map();
      for (const s of data.sessions || []) m.set(s.sid, s);
      setSessions(m);
    } catch (e) {
      console.error(e);
      setConn({ text: "snapshot失敗", level: "error" });
    }
  }, [sync]);

  useEffect(() => {
    let es = null;
    let cancelled = false;
    (async () => {
      await loadSnapshot();
      if (cancelled) return;
      es = new EventSource("/api/events");
      es.addEventListener("open", () =>
        setConn({ text: "live", level: "success" }),
      );
      es.addEventListener("error", () =>
        setConn({ text: "reconnecting…", level: "error" }),
      );
      es.addEventListener("session_update", (ev) => {
        try {
          const s = JSON.parse(ev.data);
          setSessions((prev) => {
            const next = new Map(prev);
            next.set(s.sid, s);
            return next;
          });
        } catch (e) {
          console.warn("bad session_update", e);
        }
      });
      es.addEventListener("task_update", (ev) => {
        try {
          const payload = JSON.parse(ev.data);
          setSessions((prev) => {
            const cur = prev.get(payload.sid);
            if (!cur) return prev;
            const next = new Map(prev);
            next.set(payload.sid, { ...cur, todos: payload.todos });
            return next;
          });
        } catch (e) {
          console.warn("bad task_update", e);
        }
      });
      es.addEventListener("heartbeat", (ev) => {
        try {
          const d = JSON.parse(ev.data);
          if (d?.ts) sync(d.ts);
        } catch {}
      });
    })();
    return () => {
      cancelled = true;
      if (es) es.close();
    };
  }, [loadSnapshot, sync]);

  useEffect(() => {
    const onFocus = () => loadSnapshot();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [loadSnapshot]);

  useEffect(() => {
    const handler = (e) => {
      if (e.target.matches?.("input, textarea")) return;
      if (sortedList.length === 0) return;
      const idx = sortedList.findIndex((s) => s.sid === selectedSid);
      if (e.key === "j") {
        const next = sortedList[Math.min(idx + 1, sortedList.length - 1)];
        if (next) setSelectedSid(next.sid);
      } else if (e.key === "k") {
        const prev = sortedList[Math.max(idx - 1, 0)];
        if (prev) setSelectedSid(prev.sid);
      } else if (e.key === "g") {
        setSelectedSid(sortedList[0].sid);
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [sortedList, selectedSid]);

  const selected = selectedSid ? sessions.get(selectedSid) : null;
  const now = nowAdjusted();

  const connColor =
    conn.level === "success"
      ? "#22c55e"
      : conn.level === "error"
        ? "#e53935"
        : "#98a2b3";

  return html`
    <${Layout} style=${{ height: "100vh" }}>
      <${Header}
        style=${{
          height: 44,
          lineHeight: "44px",
          padding: "0 16px",
          background: "#161a22",
          borderBottom: "1px solid #2a3140",
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
        }}
      >
        <${Title} level=${5} style=${{ margin: 0, color: "#e8ecf2" }}>
          Claude Checker
        <//>
        <${Text} style=${{ color: connColor, fontSize: 12 }}>${conn.text}<//>
      <//>
      <${Layout}>
        <${Sider}
          width=${340}
          style=${{
            background: "#161a22",
            borderInlineEnd: "1px solid #2a3140",
            overflowY: "auto",
            height: "calc(100vh - 44px)",
          }}
        >
          <${UrgentBanner}
            count=${urgentCount}
            onJump=${() => {
              if (sortedList.length > 0) setSelectedSid(sortedList[0].sid);
            }}
          />
          <${List}
            dataSource=${sortedList}
            split
            renderItem=${(s) => html`
              <${SessionRow}
                key=${s.sid}
                session=${s}
                selected=${s.sid === selectedSid}
                ageSeconds=${now - (s.last_event_ts || 0)}
                onClick=${() => setSelectedSid(s.sid)}
              />
            `}
            locale=${{
              emptyText: html`<${Empty} description="セッションなし" />`,
            }}
          />
        <//>
        <${Content}
          style=${{
            background: "#0f1115",
            overflowY: "auto",
            height: "calc(100vh - 44px)",
          }}
        >
          <${Board} session=${selected} nowAdjusted=${now} />
        <//>
      <//>
    <//>
  `;
}

function Root() {
  return html`
    <${ConfigProvider}
      theme=${{
        algorithm: theme.darkAlgorithm,
        token: {
          colorPrimary: "#5ac8fa",
          colorBgBase: "#0f1115",
          colorBgContainer: "#161a22",
          colorBgElevated: "#1f2531",
          colorBorder: "#2a3140",
          colorText: "#e8ecf2",
          colorTextSecondary: "#98a2b3",
          fontSize: 14,
          borderRadius: 6,
        },
        components: {
          Layout: {
            headerBg: "#161a22",
            siderBg: "#161a22",
            bodyBg: "#0f1115",
          },
          List: { headerBg: "#161a22" },
          Card: { colorBgContainer: "#1f2531" },
        },
      }}
    >
      <${App} />
    <//>
  `;
}

const root = createRoot(document.getElementById("root"));
root.render(html`<${Root} />`);
