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
    Html(load_index_html(state.port))
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
        anyhow::bail!("message cannot be empty");
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
        "accepted request from local ui surface",
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

fn load_index_html(port: u16) -> String {
    include_str!("ui/index.html").replace("__PORT__", &port.to_string())
}
