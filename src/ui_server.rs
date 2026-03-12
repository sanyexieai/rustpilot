use std::collections::{BTreeMap, BTreeSet, HashMap};
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

use crate::openai_compat::Message as ChatMessage;
use crate::project_tools::ProjectContext;
use crate::runtime::approval::{approval_mode_name, approval_mode_summary};
use crate::team::list_worker_endpoints;
use crate::wire::{WireRequest, WireResponse};
use crate::wire_exec::execute_ui_wire_request;

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
                .route("/api/request", post(api_request_compat))
                .route("/api/wire", post(api_wire_request))
                .route("/ws", get(ws_handler))
                .with_state(state.clone());
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("[ui-server] serve error: {}", err);
            }
        });
    }))
}

async fn index(State(state): State<UiServerState>) -> Html<String> {
    let html = load_ui_html(&state).unwrap_or_else(|err| emergency_ui_html(&state, &err));
    Html(html)
}

async fn api_status(
    State(state): State<UiServerState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    Ok(Json(build_status_payload(&state).map_err(internal_error)?))
}

async fn api_request_compat(
    State(state): State<UiServerState>,
    Json(payload): Json<UiRequestPayload>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let response = execute_ui_wire_request(
        compat_payload_to_wire_request(payload),
        &state.repo_root,
        &state.agent_id,
        "ui.http.request.received",
    )
    .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    Ok(Json(compat_response_payload(response)))
}

async fn api_wire_request(
    State(state): State<UiServerState>,
    Json(request): Json<WireRequest>,
) -> Result<Json<WireResponse>, (StatusCode, String)> {
    let response = execute_ui_wire_request(
        request,
        &state.repo_root,
        &state.agent_id,
        "ui.http.wire_request.received",
    )
    .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    Ok(Json(response))
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
        if sender.send(Message::Text(snapshot)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<UiSocketClientMessage>(&text) {
                        Ok(message) if message.msg_type == "dispatch_request" => {
                            let event = match serde_json::from_value::<UiRequestPayload>(message.payload) {
                                Ok(payload) => match execute_ui_wire_request(
                                    compat_payload_to_wire_request(payload),
                                    &state.repo_root,
                                    &state.agent_id,
                                    "ui.ws.request.received",
                                ) {
                                    Ok(response) => {
                                        serialize_ws_event(
                                            "wire.response",
                                            serde_json::to_value(response).unwrap_or_else(|_| json!({"error":"serialize failed"})),
                                        )
                                    }
                                    Err(err) => serialize_ws_event("wire.error", json!({ "error": err.to_string() })),
                                },
                                Err(err) => serialize_ws_event("wire.error", json!({ "error": err.to_string() })),
                            };
                            if let Ok(event) = event
                                && sender.send(Message::Text(event)).await.is_err()
                            {
                                break;
                            }
                        }
                        Ok(message) if message.msg_type == "wire_request" => {
                            let event = match serde_json::from_value::<WireRequest>(message.payload) {
                                Ok(request) => match execute_ui_wire_request(
                                    request,
                                    &state.repo_root,
                                    &state.agent_id,
                                    "ui.ws.wire_request.received",
                                ) {
                                    Ok(response) => serialize_ws_event("wire.response", serde_json::to_value(response).unwrap_or_else(|_| json!({"error":"serialize failed"}))),
                                    Err(err) => serialize_ws_event("wire.error", json!({ "error": err.to_string() })),
                                },
                                Err(err) => serialize_ws_event("wire.error", json!({ "error": err.to_string() })),
                            };
                            if let Ok(event) = event
                                && sender.send(Message::Text(event)).await.is_err()
                            {
                                break;
                            }
                        }
                        Ok(_) => {
                            if let Ok(event) = serialize_ws_event("error", json!({ "error": "unsupported client message type" }))
                                && sender.send(Message::Text(event)).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(err) => {
                            if let Ok(event) = serialize_ws_event("error", json!({ "error": err.to_string() }))
                                && sender.send(Message::Text(event)).await.is_err()
                            {
                                break;
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
                    && snapshot != last_snapshot
                {
                    last_snapshot = snapshot.clone();
                    if sender.send(Message::Text(snapshot)).await.is_err() {
                        break;
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
    payload: Value,
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn build_status_payload(state: &UiServerState) -> anyhow::Result<Value> {
    let project = ProjectContext::new(state.repo_root.clone())?;
    let desired_view = project.ui_page().user_request_memory()?.desired_view;
    let model = match project.system_model().snapshot()? {
        Some(model) => model,
        None => project.system_model().rebuild(&project)?,
    };
    let schema = match project.ui_schema().snapshot()? {
        Some(schema) => schema,
        None => {
            let surface = match project.ui_surface().snapshot()? {
                Some(surface) => surface,
                None => project
                    .ui_surface()
                    .rebuild_from_model(&model, &desired_view)?,
            };
            let fingerprint = format!(
                "{}:{}:{}",
                project.ui_surface().prompt_fingerprint()?,
                surface.source_fingerprint,
                desired_view,
            );
            project.ui_schema().generate_from_surface(
                &model,
                &surface,
                &desired_view,
                &fingerprint,
            )?
        }
    };
    let approval = project.approval().get_policy()?;
    let sessions = project
        .sessions()
        .list()?
        .into_iter()
        .take(8)
        .map(|item| {
            json!({
                "session_id": item.session_id,
                "label": item.label,
                "focus": item.focus,
                "status": item.status,
            })
        })
        .collect::<Vec<_>>();
    let approval_history = project
        .approval()
        .list_recent_blocks(5, None)?
        .into_iter()
        .map(|item| {
            json!({
                "ts": item.ts,
                "actor_id": item.actor_id,
                "tool_name": item.tool_name,
                "command": item.command,
                "reason_code": item.reason_code,
                "message": item.message,
            })
        })
        .collect::<Vec<_>>();
    let chat_ui = build_chat_ui_payload(&project, &model)?;

    Ok(json!({
        "agent_id": state.agent_id,
        "port": state.port,
        "approval_mode": approval_mode_name(approval.mode),
        "approval_summary": approval_mode_summary(approval.mode),
        "approval_allowed_tools": crate::runtime::approval::approval_allowed_tools(approval.mode),
        "approval_last_block": approval.last_block.as_ref().map(|block| json!({
            "ts": block.ts,
            "actor_id": block.actor_id,
            "tool_name": block.tool_name,
            "command": block.command,
            "reason_code": block.reason_code,
            "message": block.message,
        })),
        "approval_history": approval_history,
        "sessions": sessions,
        "chat_ui": chat_ui,
        "schema": schema,
        "model": model,
    }))
}

fn build_chat_ui_payload(
    project: &ProjectContext,
    model: &crate::project_tools::SystemModel,
) -> anyhow::Result<Value> {
    let profiles = project.agents().profiles()?;
    let states = project.agents().states()?;
    let state_map = states
        .iter()
        .map(|item| (item.agent_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let profile_map = profiles
        .iter()
        .map(|item| (item.agent_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let resident_map = model
        .residents
        .iter()
        .map(|item| (item.agent_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let worker_endpoints = list_worker_endpoints(project.repo_root()).unwrap_or_default();
    let worker_map = worker_endpoints
        .iter()
        .map(|item| (item.owner.as_str(), item))
        .collect::<HashMap<_, _>>();
    let sessions = project.sessions().list()?;
    let mut agent_ids = BTreeSet::new();
    agent_ids.insert("lead".to_string());
    for profile in &profiles {
        agent_ids.insert(profile.agent_id.clone());
    }
    for state in &states {
        agent_ids.insert(state.agent_id.clone());
    }
    for resident in &model.residents {
        agent_ids.insert(resident.agent_id.clone());
    }
    for worker in &worker_endpoints {
        agent_ids.insert(worker.owner.clone());
    }

    let agents = agent_ids
        .into_iter()
        .map(|agent_id| {
            let state = state_map.get(agent_id.as_str()).copied();
            let profile = profile_map.get(agent_id.as_str()).copied();
            let resident = resident_map.get(agent_id.as_str()).copied();
            let worker = worker_map.get(agent_id.as_str()).copied();
            let transcript_session_id = transcript_session_id_for_agent(&agent_id, &sessions);
            let transcript = transcript_session_id
                .as_deref()
                .map(|session_id| load_transcript(project, session_id))
                .transpose()?
                .unwrap_or_default();
            let transcript_roles = transcript
                .iter()
                .filter_map(|item| item.get("role").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let transcript_preview = transcript
                .last()
                .and_then(|item| item.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(90)
                .collect::<String>();
            Ok(json!({
                "agent_id": agent_id,
                "display_name": display_name_for_agent(&agent_id, profile),
                "group_member": agent_id != "lead",
                "role": profile
                    .map(|item| item.role.clone())
                    .or_else(|| resident.map(|item| item.role.clone()))
                    .unwrap_or_else(|| "agent".to_string()),
                "status": state
                    .map(|item| item.status.clone())
                    .or_else(|| resident.map(|item| item.status.clone()))
                    .or_else(|| worker.map(|item| item.status.clone()))
                    .unwrap_or_else(|| "unknown".to_string()),
                "mission": profile.map(|item| item.mission.clone()).unwrap_or_default(),
                "scope": profile.map(|item| item.scope.clone()).unwrap_or_default(),
                "forbidden": profile.map(|item| item.forbidden.clone()).unwrap_or_default(),
                "note": state.and_then(|item| item.note.clone()).unwrap_or_default(),
                "current_task_id": state.and_then(|item| item.current_task_id),
                "channel": state.and_then(|item| item.channel.clone()).unwrap_or_default(),
                "target": state.and_then(|item| item.target.clone()).unwrap_or_default(),
                "backlog": resident.map(|item| item.backlog).unwrap_or(0),
                "last_error": resident.map(|item| item.last_error.clone()).unwrap_or_default(),
                "last_action": resident.map(|item| item.last_action.clone()).unwrap_or_default(),
                "last_summary": resident.map(|item| item.last_summary.clone()).unwrap_or_default(),
                "prompt_strategy": resident.map(|item| item.prompt_strategy.clone()).unwrap_or_default(),
                "energy": resident.map(|item| item.energy.clone()).unwrap_or_default(),
                "worker_channel": worker.map(|item| item.channel.clone()).unwrap_or_default(),
                "worker_target": worker.map(|item| item.target.clone()).unwrap_or_default(),
                "runtime_source": if worker.is_some() {
                    "worker_endpoint"
                } else if resident.is_some() {
                    "resident_runtime"
                } else if transcript_session_id.is_some() {
                    "session_history"
                } else {
                    "agent_registry"
                },
                "transcript_session_id": transcript_session_id,
                "transcript_available": !transcript.is_empty(),
                "transcript_roles": transcript_roles,
                "transcript_message_count": transcript.len(),
                "transcript_preview": transcript_preview,
                "transcript": transcript,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let recent_mail = project.mailbox().list_recent(80)?;
    let timeline = recent_mail
        .into_iter()
        .map(|item| {
            let kind = mailbox_kind(&item.from, &item.to, &item.msg_type);
            json!({
                "msg_id": item.msg_id,
                "from": item.from,
                "to": item.to,
                "type": item.msg_type,
                "kind": kind,
                "direction": mailbox_direction(&item.from, &item.to),
                "message": item.message,
                "ts": item.ts,
                "task_id": item.task_id,
                "trace_id": item.trace_id,
            })
        })
        .collect::<Vec<_>>();

    let lead_session_id = transcript_session_id_for_agent("lead", &sessions);
    let lead_transcript = lead_session_id
        .as_deref()
        .map(|session_id| load_transcript(project, session_id))
        .transpose()?
        .unwrap_or_default();
    let group_members = agents
        .iter()
        .filter_map(|item| item.get("agent_id").cloned())
        .filter(|agent_id| agent_id.as_str() != Some("lead"))
        .collect::<Vec<_>>();

    let agent_details = agents
        .iter()
        .filter_map(|item| {
            item.get("agent_id")
                .and_then(Value::as_str)
                .map(|agent_id| (agent_id.to_string(), item.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(json!({
        "main_friend": {
            "agent_id": "lead",
            "display_name": "Main",
            "chat_kind": "direct",
            "default_target": "lead",
            "session_id": lead_session_id,
            "transcript_message_count": lead_transcript.len(),
            "transcript": lead_transcript,
        },
        "group_chat": {
            "group_id": "agent-team",
            "title": "Agent Team",
            "chat_kind": "group",
            "primary_actor": "lead",
            "member_ids": group_members,
            "member_count": agents.iter().filter(|item| item.get("agent_id").and_then(Value::as_str) != Some("lead")).count(),
            "timeline": timeline,
        },
        "agents": agents,
        "agent_details": agent_details,
    }))
}

fn transcript_session_id_for_agent(
    agent_id: &str,
    sessions: &[crate::project_tools::SessionRecord],
) -> Option<String> {
    if agent_id == "lead" {
        if sessions.iter().any(|item| item.session_id == "cli-main") {
            return Some("cli-main".to_string());
        }
        return sessions
            .iter()
            .find(|item| item.focus == "lead")
            .map(|item| item.session_id.clone());
    }
    sessions
        .iter()
        .find(|item| item.session_id == agent_id)
        .map(|item| item.session_id.clone())
}

fn load_transcript(project: &ProjectContext, session_id: &str) -> anyhow::Result<Vec<Value>> {
    let messages = project.sessions().load_messages(session_id)?;
    Ok(messages
        .into_iter()
        .filter_map(message_to_json)
        .collect::<Vec<_>>())
}

fn message_to_json(message: ChatMessage) -> Option<Value> {
    let content = message.content?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(json!({
        "role": message.role,
        "kind": transcript_kind(&message.role),
        "content": trimmed,
    }))
}

fn display_name_for_agent(
    agent_id: &str,
    profile: Option<&crate::project_tools::AgentProfile>,
) -> String {
    if agent_id == "lead" {
        return "Main".to_string();
    }
    profile
        .map(|item| item.role.clone())
        .filter(|item| !item.is_empty())
        .map(|role| format!("{agent_id} ({role})"))
        .unwrap_or_else(|| agent_id.to_string())
}

fn mailbox_direction(from: &str, to: &str) -> &'static str {
    if from == "lead" {
        "main_to_agent"
    } else if to == "lead" {
        "agent_to_main"
    } else {
        "agent_to_agent"
    }
}

fn mailbox_kind(from: &str, to: &str, msg_type: &str) -> &'static str {
    if msg_type.starts_with("task.") {
        "task_update"
    } else if from == "lead" || to == "lead" {
        "chat"
    } else {
        "system"
    }
}

fn transcript_kind(role: &str) -> &'static str {
    match role {
        "user" => "prompt",
        "assistant" => "llm_output",
        "tool" => "tool_result",
        "system" => "system_prompt",
        _ => "message",
    }
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

fn compat_payload_to_wire_request(payload: UiRequestPayload) -> WireRequest {
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
    WireRequest::ChatSend {
        input: format!("[{}] {}", priority, payload.message.trim()),
        focus: Some(payload.target.unwrap_or_else(|| "ui".to_string())),
    }
}

fn compat_response_payload(response: WireResponse) -> Value {
    match response {
        WireResponse::Ack { message } => match serde_json::from_str::<Value>(&message) {
            Ok(value) => value,
            Err(_) => json!({
                "queued": true,
                "message": message
            }),
        },
        WireResponse::Error { message } => json!({
            "queued": false,
            "error": message
        }),
        other => json!({
            "queued": false,
            "error": format!("unsupported legacy response: {:?}", other)
        }),
    }
}

fn serialize_ws_event(event_type: &str, payload: Value) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&json!({
        "type": event_type,
        "payload": payload,
    }))?)
}

fn load_ui_html(state: &UiServerState) -> anyhow::Result<String> {
    let project = ProjectContext::new(state.repo_root.clone())?;
    let desired_view = project.ui_page().user_request_memory()?.desired_view;
    let model = match project.system_model().snapshot()? {
        Some(model) => model,
        None => project.system_model().rebuild(&project)?,
    };
    let surface = match project.ui_surface().snapshot()? {
        Some(surface) => surface,
        None => project
            .ui_surface()
            .rebuild_from_model(&model, &desired_view)?,
    };
    let prompt_text = project.ui_surface().prompt_text()?;
    let schema_fingerprint = format!(
        "{}:{}:{}",
        project.ui_surface().prompt_fingerprint()?,
        surface.source_fingerprint,
        desired_view,
    );
    let schema = if project.ui_schema().needs_refresh(&schema_fingerprint)? {
        project
            .ui_schema()
            .generate_with_ui_agent(
                &model,
                &surface,
                &prompt_text,
                &desired_view,
                &schema_fingerprint,
            )
            .or_else(|_| {
                project.ui_schema().generate_from_surface(
                    &model,
                    &surface,
                    &desired_view,
                    &schema_fingerprint,
                )
            })?
    } else {
        project
            .ui_schema()
            .snapshot()?
            .ok_or_else(|| anyhow::anyhow!("ui schema missing after refresh check"))?
    };

    let page_fingerprint = format!(
        "{}:{}:{}:{}",
        schema.source_fingerprint,
        project.ui_page().prompt_fingerprint()?,
        project.ui_page().design_rules_fingerprint()?,
        project.ui_page().user_request_fingerprint()?
    );
    if !project.ui_page().needs_refresh(&page_fingerprint)?
        && let Some(page) = project.ui_page().snapshot()?
    {
        return Ok(page.html.replace("__PORT__", &state.port.to_string()));
    }

    let context = project
        .ui_page()
        .build_context(&model, &surface, &schema, &page_fingerprint)?;
    let fallback_page =
        project
            .ui_page()
            .generate_from_context(&context, state.port, &page_fingerprint)?;
    let page = project
        .ui_page()
        .generate_with_ui_agent(&context, &fallback_page.html, &page_fingerprint)
        .unwrap_or(fallback_page);
    Ok(page.html.replace("__PORT__", &state.port.to_string()))
}

fn emergency_ui_html(state: &UiServerState, err: &anyhow::Error) -> String {
    let error_text = err.to_string();
    format!(
        "<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
  <title>Rustpilot UI Bootstrap</title>
  <style>
    body {{ margin:0; font-family:Segoe UI, sans-serif; background:#0f1115; color:#edf2f7; }}
    main {{ max-width:900px; margin:0 auto; padding:32px 20px; }}
    section {{ background:#171b22; border:1px solid #2a3240; border-radius:18px; padding:20px; }}
    h1 {{ margin-top:0; }}
    p, li {{ color:#b3bdca; line-height:1.5; }}
    code, pre {{ font-family:Consolas, monospace; }}
    pre {{ white-space:pre-wrap; word-break:break-word; background:#11151b; padding:14px; border-radius:12px; }}
  </style>
</head>
<body>
  <main>
    <section>
      <h1>Rustpilot UI bootstrap fallback</h1>
      <p>The generated management page is not ready yet. The backend is still running and the protocol endpoints remain available.</p>
      <ul>
        <li><code>GET /api/status</code></li>
        <li><code>POST /api/wire</code></li>
        <li><code>GET /ws</code></li>
      </ul>
      <p>Agent: <code>{}</code></p>
      <p>Port: <code>{}</code></p>
      <pre>{}</pre>
    </section>
  </main>
</body>
</html>",
        escape_html(&state.agent_id),
        state.port,
        escape_html(&error_text)
    )
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}
