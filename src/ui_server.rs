use std::path::PathBuf;
use std::thread;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{self, Duration};

use crate::project_tools::ProjectContext;

#[derive(Clone)]
struct UiServerState {
    repo_root: PathBuf,
    agent_id: String,
    port: u16,
}

pub fn spawn_ui_server(
    repo_root: PathBuf,
    agent_id: String,
    port: u16,
) -> anyhow::Result<thread::JoinHandle<()>> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    let state = UiServerState {
        repo_root,
        agent_id,
        port,
    };
    Ok(thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!("[ui-server] failed to build runtime: {}", err);
                return;
            }
        };

        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!("[ui-server] failed to convert listener: {}", err);
                    return;
                }
            };
            let app = Router::new()
                .route("/", get(index))
                .route("/api/status", get(api_status))
                .route("/api/request", post(api_request))
                .route("/ws", get(ws_handler))
                .with_state(state.clone());
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("[ui-server] serve error: {}", err);
            }
        });
    }))
}

async fn index(State(state): State<UiServerState>) -> Html<String> {
    Html(render_index(state.port))
}

async fn api_status(
    State(state): State<UiServerState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    Ok(Json(build_status_payload(&state).map_err(internal_error)?))
}

async fn api_request(
    State(state): State<UiServerState>,
    Json(payload): Json<UiRequestPayload>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = dispatch_request(&state, payload, "ui.http.request.received")
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    Ok(Json(result))
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<UiServerState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ui_socket(socket, state))
}

async fn ui_socket(socket: WebSocket, state: UiServerState) {
    let (mut sender, mut receiver) = socket.split();
    let mut ticker = time::interval(Duration::from_secs(2));
    let mut last_snapshot = String::new();

    if let Ok(snapshot) = build_status_payload(&state)
        .and_then(stable_snapshot_payload)
        .and_then(|value| serialize_ws_event("system.snapshot", value))
    {
        last_snapshot = snapshot.clone();
        if sender.send(Message::Text(snapshot.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<UiSocketClientMessage>(&text) {
                        Ok(message) if message.msg_type == "dispatch_request" => {
                            let event = match dispatch_request(&state, message.payload, "ui.ws.request.received") {
                                Ok(payload) => serialize_ws_event("request.accepted", payload),
                                Err(err) => serialize_ws_event("request.rejected", json!({ "error": err.to_string() })),
                            };
                            if let Ok(event) = event {
                                if sender.send(Message::Text(event.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Ok(_) => {
                            if let Ok(event) = serialize_ws_event("error", json!({ "error": "unsupported client message type" })) {
                                if sender.send(Message::Text(event.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(err) => {
                            if let Ok(event) = serialize_ws_event("error", json!({ "error": err.to_string() })) {
                                if sender.send(Message::Text(event.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if sender.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            _ = ticker.tick() => {
                if let Ok(snapshot) = build_status_payload(&state)
                    .and_then(stable_snapshot_payload)
                    .and_then(|value| serialize_ws_event("system.snapshot", value))
                {
                    if snapshot != last_snapshot {
                        last_snapshot = snapshot.clone();
                        if sender.send(Message::Text(snapshot.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct UiRequestPayload {
    message: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UiSocketClientMessage {
    #[serde(rename = "type")]
    msg_type: String,
    payload: UiRequestPayload,
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn build_status_payload(state: &UiServerState) -> anyhow::Result<Value> {
    let project = ProjectContext::new(state.repo_root.clone())?;
    let model = match project.system_model().snapshot()? {
        Some(model) => model,
        None => project.system_model().rebuild(&project)?,
    };
    let schema = match project.ui_schema().snapshot()? {
        Some(schema) => schema,
        None => {
            let surface = match project.ui_surface().snapshot()? {
                Some(surface) => surface,
                None => project.ui_surface().rebuild_from_model(&model)?,
            };
            let fingerprint = format!(
                "{}:{}",
                project.ui_surface().prompt_fingerprint()?,
                surface.source_fingerprint,
            );
            project
                .ui_schema()
                .generate_from_surface(&model, &surface, &fingerprint)?
        }
    };

    Ok(json!({
        "agent_id": state.agent_id,
        "port": state.port,
        "schema": schema,
        "model": model,
    }))
}

fn stable_snapshot_payload(mut payload: Value) -> anyhow::Result<Value> {
    if let Some(schema) = payload
        .get_mut("schema")
        .and_then(|item| item.as_object_mut())
    {
        schema.remove("generated_at");
    }
    if let Some(model) = payload
        .get_mut("model")
        .and_then(|item| item.as_object_mut())
    {
        model.remove("generated_at");
        if let Some(residents) = model
            .get_mut("residents")
            .and_then(|item| item.as_array_mut())
        {
            for resident in residents {
                if let Some(object) = resident.as_object_mut() {
                    object.remove("loop_ms");
                }
            }
        }
    }
    Ok(payload)
}

fn dispatch_request(
    state: &UiServerState,
    payload: UiRequestPayload,
    decision_action: &str,
) -> anyhow::Result<Value> {
    let message = payload.message.trim();
    if message.is_empty() {
        anyhow::bail!("请输入内容");
    }

    let target = payload.target.as_deref().unwrap_or("ui").trim().to_string();
    let msg_type = match target.as_str() {
        "concierge" => "user.request",
        "reviewer" => "proposal.request",
        _ => "ui.request",
    };
    let priority = payload
        .priority
        .as_deref()
        .unwrap_or("medium")
        .trim()
        .to_lowercase();
    let priority = match priority.as_str() {
        "critical" | "high" | "medium" | "low" => priority,
        _ => "medium".to_string(),
    };
    let content = format!("[{}] {}", priority, message);

    let project = ProjectContext::new(state.repo_root.clone())?;
    project.mailbox().send_typed(
        "ui-web", &target, msg_type, &content, None, None, false, None,
    )?;
    let _ = project.decisions().append(
        "ui-web",
        decision_action,
        None,
        None,
        "从本地系统面板投递请求",
        &format!("target={} type={} priority={}", target, msg_type, priority),
    );

    Ok(json!({
        "queued": true,
        "target": target,
        "priority": priority,
        "message": content
    }))
}

fn serialize_ws_event(event_type: &str, payload: Value) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&json!({
        "type": event_type,
        "payload": payload,
    }))?)
}

fn render_index(port: u16) -> String {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Rustpilot 控制台</title>
  <style>
    :root {
      --bg: #f3ecd9;
      --panel: rgba(255,255,255,0.82);
      --ink: #172025;
      --muted: #62707a;
      --line: rgba(23,32,37,0.12);
      --accent: #cb5c35;
      --accent-2: #0f7a65;
      --chip: #f3ede2;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: "Microsoft YaHei", "PingFang SC", "Segoe UI", sans-serif;
      color: var(--ink);
      background:
        radial-gradient(circle at top left, rgba(203,92,53,0.18), transparent 28%),
        radial-gradient(circle at top right, rgba(15,122,101,0.14), transparent 25%),
        linear-gradient(180deg, #f9f3e9 0%, #efe6d7 100%);
    }
    .wrap { max-width: 1320px; margin: 0 auto; padding: 28px 18px 42px; }
    .hero { display: grid; gap: 10px; margin-bottom: 18px; }
    .hero h1 { margin: 0; font-size: clamp(30px, 5vw, 62px); line-height: 0.92; letter-spacing: -0.045em; }
    .hero p { margin: 0; color: var(--muted); max-width: 820px; line-height: 1.7; }
    .hero-bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
    .hero-actions { display: flex; align-items: center; gap: 10px; }
    .hero-actions button { width: auto; min-width: 110px; }
    .hero-actions .meta { margin-top: 0; font-size: 12px; }
    .stack { display: grid; gap: 16px; }
    .stats { display: grid; grid-template-columns: repeat(6, 1fr); gap: 12px; }
    .pill, .card { background: var(--panel); backdrop-filter: blur(10px); border: 1px solid var(--line); box-shadow: 0 12px 32px rgba(23,32,37,0.07); }
    .pill { border-radius: 18px; padding: 12px 14px; }
    .pill small { color: var(--muted); display: block; letter-spacing: 0.05em; }
    .pill strong { display: block; margin-top: 5px; font-size: 23px; }
    .card { border-radius: 24px; padding: 18px; }
    .card h2 { margin: 0 0 8px; font-size: 16px; letter-spacing: 0.04em; }
    .card p { margin: 0 0 14px; color: var(--muted); font-size: 13px; line-height: 1.6; }
    .resident-grid { display: grid; gap: 12px; }
    .resident { border: 1px solid var(--line); border-radius: 18px; padding: 14px; background: rgba(255,255,255,0.72); }
    .resident .head { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
    .resident strong { font-size: 18px; }
    .badge { display: inline-flex; align-items: center; padding: 5px 10px; border-radius: 999px; background: var(--chip); color: var(--muted); font-size: 12px; border: 1px solid var(--line); }
    .kv { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin-top: 10px; }
    .kv div { background: rgba(255,255,255,0.56); border: 1px solid var(--line); border-radius: 12px; padding: 10px; }
    .kv small { display: block; color: var(--muted); margin-bottom: 5px; }
    .composer textarea, .composer select, .composer button {
      width: 100%; border-radius: 14px; border: 1px solid var(--line); padding: 12px 14px; font: inherit;
      background: rgba(255,255,255,0.92); color: var(--ink);
    }
    textarea { min-height: 148px; resize: vertical; margin-bottom: 10px; }
    .row { display: grid; grid-template-columns: 1fr 140px 150px; gap: 10px; margin-bottom: 10px; }
    button { background: linear-gradient(135deg, var(--accent), #e28a42); color: white; border: none; font-weight: 700; cursor: pointer; }
    .feed { list-style: none; padding: 0; margin: 0; display: grid; gap: 10px; }
    .feed li { border: 1px solid var(--line); border-radius: 16px; padding: 12px 13px; background: rgba(255,255,255,0.68); }
    .ok { color: var(--accent-2); min-height: 20px; font-size: 13px; margin-top: 6px; }
    @media (max-width: 980px) { .stats { grid-template-columns: repeat(3, 1fr); } .row { grid-template-columns: 1fr; } .kv { grid-template-columns: 1fr; } }
    @media (max-width: 640px) { .stats { grid-template-columns: repeat(2, 1fr); } }
  </style>
</head>
<body>
  <div class="wrap">
    <section class="hero">
      <div class="hero-bar">
        <h1 id="title">Rustpilot 控制台</h1>
        <div class="hero-actions">
          <button id="reconnect" type="button">重新连接</button>
          <span class="meta" id="refresh-state">实时通道连接中...</span>
        </div>
      </div>
      <p id="subtitle">加载中...</p>
    </section>
    <section id="sections" class="stack"></section>
  </div>

  <script>
    function labelMap(section) {
      const map = {};
      (section.labels || []).forEach(item => { map[item.key] = item.text; });
      return map;
    }

    function card(section, body) {
      const desc = section.description ? `<p>${section.description}</p>` : '';
      return `<div class="card"><h2>${section.title}</h2>${desc}${body}</div>`;
    }

    function renderMetrics(section, summary) {
      const labels = labelMap(section);
      const items = [
        ['resident_count', summary.resident_count],
        ['pending_tasks', summary.pending_tasks],
        ['running_tasks', summary.running_tasks],
        ['blocked_tasks', summary.blocked_tasks],
        ['completed_tasks', summary.completed_tasks],
        ['open_proposals', summary.open_proposals],
      ];
      const body = `<div class="stats">${items.map(([key, value]) =>
        `<div class="pill"><small>${labels[key] || key}</small><strong>${value ?? 0}</strong></div>`
      ).join('')}</div>`;
      return card(section, body);
    }

    function renderAlerts(section, alerts) {
      const labels = labelMap(section);
      const body = !alerts.length
        ? `<div class="badge">${section.empty_text || 'No alerts'}</div>`
        : `<ul class="feed">${alerts.map(item => `
            <li>
              <strong>${item.summary}</strong>
              <div class="kv">
                <div><small>${labels.severity || 'severity'}</small>${item.severity || '-'}</div>
                <div><small>${labels.detail || 'detail'}</small>${item.detail || '-'}</div>
              </div>
            </li>`).join('')}</ul>`;
      return card(section, body);
    }

    function renderResidents(section, residents) {
      const labels = labelMap(section);
      const body = !residents.length
        ? `<div class="badge">${section.empty_text || 'No residents'}</div>`
        : `<div class="resident-grid">${residents.map(item => `
            <div class="resident">
              <div class="head">
                <strong>${item.agent_id}</strong>
                <span class="badge">${item.role || '-'}</span>
              </div>
              <div class="kv">
                <div><small>${labels.status || 'status'}</small>${item.status || '-'}</div>
                <div><small>${labels.energy || 'energy'}</small>${item.energy || '-'}</div>
                <div><small>${labels.backlog || 'backlog'}</small>${item.backlog ?? 0}</div>
                <div><small>${labels.loop_ms || 'loop_ms'}</small>${item.loop_ms ?? 0} ms</div>
                <div><small>${labels.budget || 'budget'}</small>${item.budget_used ?? 0}/${item.budget_limit ?? 0}</div>
                <div><small>${labels.endpoint || 'endpoint'}</small>${item.port ? `127.0.0.1:${item.port}` : '-'}</div>
                <div><small>${labels.mode || 'mode'}</small>${item.runtime_mode || '-'}/${item.behavior_mode || '-'}</div>
                <div><small>${labels.note || 'note'}</small>${item.note || '-'}</div>
                <div><small>${labels.last_action || 'last_action'}</small>${item.last_action || '-'}</div>
                <div><small>${labels.last_error || 'last_error'}</small>${item.last_error || '-'}</div>
              </div>
            </div>`).join('')}</div>`;
      return card(section, body);
    }

    function renderComposer(section) {
      const labels = labelMap(section);
      const options = (section.target_options || []).map(item =>
        `<option value="${item.value}">${item.label}</option>`
      ).join('');
      const body = `
        <div class="composer">
          <textarea id="message" placeholder="${labels.placeholder || ''}"></textarea>
          <div class="row">
            <select id="priority">
              <option value="critical">${labels.priority_critical || 'critical'}</option>
              <option value="high">${labels.priority_high || 'high'}</option>
              <option value="medium" selected>${labels.priority_medium || 'medium'}</option>
              <option value="low">${labels.priority_low || 'low'}</option>
            </select>
            <select id="target">${options}</select>
            <button id="submit" type="button">${labels.submit || 'submit'}</button>
          </div>
          <div class="ok" id="result"></div>
        </div>`;
      return card(section, body);
    }

    function renderTasks(section, tasks) {
      const labels = labelMap(section);
      const body = !tasks.length
        ? `<div class="badge">${section.empty_text || 'No tasks'}</div>`
        : `<ul class="feed">${tasks.map(item => `
            <li>
              <strong>#${item.id} ${item.subject}</strong>
              <div class="kv">
                <div><small>${labels.status || 'status'}</small>${item.status || '-'}</div>
                <div><small>${labels.role || 'role'}</small>${item.role || '-'}</div>
                <div><small>${labels.owner || 'owner'}</small>${item.owner || '-'}</div>
                <div><small>priority</small>${item.priority || '-'}</div>
              </div>
            </li>`).join('')}</ul>`;
      return card(section, body);
    }

    function renderProposals(section, proposals) {
      const labels = labelMap(section);
      const body = !proposals.length
        ? `<div class="badge">${section.empty_text || 'No proposals'}</div>`
        : `<ul class="feed">${proposals.map(item => `
            <li>
              <strong>#${item.id} ${item.title}</strong>
              <div class="kv">
                <div><small>${labels.score || 'score'}</small>${item.score ?? 0}</div>
                <div><small>${labels.source || 'source'}</small>${item.source || '-'}</div>
                <div><small>${labels.trigger || 'trigger'}</small>${item.trigger || '-'}</div>
                <div><small>priority</small>${item.priority || '-'}</div>
              </div>
            </li>`).join('')}</ul>`;
      return card(section, body);
    }

    function renderDecisions(section, decisions) {
      const labels = labelMap(section);
      const body = !decisions.length
        ? `<div class="badge">${section.empty_text || 'No decisions'}</div>`
        : `<ul class="feed">${decisions.map(item => `
            <li>
              <strong>${item.agent_id} · ${item.action}</strong>
              <div class="kv">
                <div><small>${labels.summary || 'summary'}</small>${item.summary || '-'}</div>
                <div><small>${labels.reason || 'reason'}</small>${item.reason || '-'}</div>
              </div>
            </li>`).join('')}</ul>`;
      return card(section, body);
    }

    function renderSection(section, model) {
      switch (section.kind) {
        case 'metrics': return renderMetrics(section, model.summary || {});
        case 'alerts': return renderAlerts(section, model.alerts || []);
        case 'residents': return renderResidents(section, model.residents || []);
        case 'composer': return renderComposer(section);
        case 'tasks': return renderTasks(section, model.tasks || []);
        case 'proposals': return renderProposals(section, model.proposals || []);
        case 'decisions': return renderDecisions(section, model.decisions || []);
        default: return '';
      }
    }

    function bindComposer(schema) {
      const submit = document.getElementById('submit');
      if (!submit || submit.dataset.bound === '1') return;
      submit.dataset.bound = '1';
      const composer = (schema.sections || []).find(item => item.kind === 'composer') || { labels: [] };
      const labels = labelMap(composer);
      submit.addEventListener('click', () => {
        const message = document.getElementById('message').value.trim();
        const priority = document.getElementById('priority').value;
        const target = document.getElementById('target').value;
        if (!message) {
          document.getElementById('result').textContent = labels.error_empty || '请输入内容';
          return;
        }
        if (!window.uiSocket || window.uiSocket.readyState !== WebSocket.OPEN) {
          document.getElementById('result').textContent = labels.error_request || '实时通道未连接';
          return;
        }
        window.uiSocket.send(JSON.stringify({
          type: 'dispatch_request',
          payload: { message, priority, target }
        }));
      });
    }

    function isComposerEditing() {
      const active = document.activeElement;
      if (!active) return false;
      return active.id === 'message' || active.id === 'priority' || active.id === 'target';
    }

    function sectionPayload(section, model) {
      switch (section.kind) {
        case 'metrics': return model.summary || {};
        case 'alerts': return model.alerts || [];
        case 'residents': return model.residents || [];
        case 'composer': return { target_options: section.target_options || [], labels: section.labels || [] };
        case 'tasks': return model.tasks || [];
        case 'proposals': return model.proposals || [];
        case 'decisions': return model.decisions || [];
        default: return {};
      }
    }

    function sameJson(a, b) {
      return JSON.stringify(a) === JSON.stringify(b);
    }

    const pendingSnapshot = { value: null };
    const currentSnapshot = { value: null };

    function ensureSectionShells(schema) {
      const root = document.getElementById('sections');
      const wanted = (schema.sections || []).map(section => section.id);
      Array.from(root.querySelectorAll('[data-section-id]')).forEach(node => {
        if (!wanted.includes(node.getAttribute('data-section-id'))) {
          node.remove();
        }
      });
      (schema.sections || []).forEach(section => {
        let node = root.querySelector(`[data-section-id="${section.id}"]`);
        if (!node) {
          node = document.createElement('div');
          node.setAttribute('data-section-id', section.id);
        }
        root.appendChild(node);
      });
    }

    function patchSections(schema, model) {
      ensureSectionShells(schema);
      const previous = currentSnapshot.value || { schema: {}, model: {} };
      (schema.sections || []).forEach(section => {
        if (section.kind === 'composer' && isComposerEditing()) {
          return;
        }
        const prevSection = (previous.schema.sections || []).find(item => item.id === section.id);
        const nextPayload = sectionPayload(section, model);
        const prevPayload = prevSection ? sectionPayload(prevSection, previous.model || {}) : null;
        if (prevSection && sameJson(prevSection, section) && sameJson(prevPayload, nextPayload)) {
          return;
        }
        const node = document.querySelector(`[data-section-id="${section.id}"]`);
        if (node) {
          node.innerHTML = renderSection(section, model);
        }
      });
    }

    function applyPendingSnapshot() {
      if (pendingSnapshot.value && !isComposerEditing()) {
        const data = pendingSnapshot.value;
        pendingSnapshot.value = null;
        renderFromPayload(data);
        const state = document.getElementById('refresh-state');
        if (state) state.textContent = '实时通道已连接';
      }
    }

    function renderFromPayload(data) {
      const previous = currentSnapshot.value;
      if (!previous || previous.schema?.title !== data.schema?.title) {
        document.getElementById('title').textContent = data.schema?.title || 'Rustpilot 控制台';
      }
      if (!previous || previous.schema?.subtitle !== data.schema?.subtitle) {
        document.getElementById('subtitle').innerHTML =
          `${data.schema?.subtitle || ''} <br>访问地址：<code>127.0.0.1:__PORT__</code>`;
      }
      patchSections(data.schema || {}, data.model || {});
      currentSnapshot.value = data;
      bindComposer(data.schema || {});
      const message = document.getElementById('message');
      const priority = document.getElementById('priority');
      const target = document.getElementById('target');
      if (message) message.addEventListener('blur', applyPendingSnapshot);
      if (priority) priority.addEventListener('blur', applyPendingSnapshot);
      if (target) target.addEventListener('blur', applyPendingSnapshot);
    }

    function connectSocket() {
      const state = document.getElementById('refresh-state');
      const protocol = location.protocol === 'https:' ? 'wss' : 'ws';
      const socket = new WebSocket(`${protocol}://${location.host}/ws`);
      window.uiSocket = socket;
      if (state) state.textContent = '实时通道连接中...';

      socket.addEventListener('open', () => {
        if (state) state.textContent = '实时通道已连接';
      });

      socket.addEventListener('message', (event) => {
        const data = JSON.parse(event.data);
        if (data.type === 'system.snapshot') {
          if (isComposerEditing()) {
            pendingSnapshot.value = data.payload || {};
            if (state) state.textContent = '检测到正在输入，更新已暂存';
          } else {
            renderFromPayload(data.payload || {});
          }
          return;
        }
        if (data.type === 'request.accepted') {
          const payload = data.payload || {};
          document.getElementById('result').textContent =
            `已投递到 ${payload.target || ''}：${payload.message || ''}`;
          document.getElementById('message').value = '';
          applyPendingSnapshot();
          return;
        }
        if (data.type === 'request.rejected' || data.type === 'error') {
          document.getElementById('result').textContent =
            (data.payload && data.payload.error) || '请求失败';
        }
      });

      socket.addEventListener('close', () => {
        if (state) state.textContent = '实时通道已断开';
      });

      socket.addEventListener('error', () => {
        if (state) state.textContent = '实时通道异常';
      });
    }

    document.getElementById('reconnect').addEventListener('click', () => {
      if (window.uiSocket && window.uiSocket.readyState === WebSocket.OPEN) {
        window.uiSocket.close();
      }
      connectSocket();
    });

    connectSocket();
  </script>
</body>
</html>"#
        .replace("__PORT__", &port.to_string())
}
