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
use crate::runtime::approval::{approval_mode_name, approval_mode_summary};
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
    Html(load_index_html(state.port))
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

fn load_index_html(port: u16) -> String {
    include_str!("ui/index.html").replace("__PORT__", &port.to_string())
}
