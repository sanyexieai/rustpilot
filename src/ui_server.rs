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
    let chat_ui = build_chat_ui_payload(&project, &model, &state.agent_id)?;
    let ui_page_ready = project
        .ui_page()
        .snapshot()?
        .map(|page| !is_bootstrap_shell(&page.html))
        .unwrap_or(false);
    let process_tree = chat_ui
        .get("process_tree")
        .cloned()
        .unwrap_or_else(|| json!({ "root_actor_id": state.agent_id, "roots": [], "nodes": [] }));

    Ok(json!({
        "agent_id": state.agent_id,
        "port": state.port,
        "launch_mode": {
            "requested_mode": model.summary.launch_mode,
            "description": model.summary.launch_mode_description,
            "effective_mode": model.summary.launch_effective_mode,
            "backend": model.summary.launch_backend,
            "note": model.summary.launch_backend_note,
        },
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
        "ui_page_ready": ui_page_ready,
        "process_tree": process_tree,
        "chat_ui": chat_ui,
        "schema": schema,
        "model": model,
    }))
}

fn build_chat_ui_payload(
    project: &ProjectContext,
    model: &crate::project_tools::SystemModel,
    preferred_actor_id: &str,
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
    let launch_map = model
        .launches
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
    if !preferred_actor_id.trim().is_empty() {
        agent_ids.insert(preferred_actor_id.trim().to_string());
    }
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
    let primary_actor_id = normalize_primary_actor_id(preferred_actor_id, &agent_ids);

    let agents = agent_ids
        .into_iter()
        .map(|agent_id| {
            let state = state_map.get(agent_id.as_str()).copied();
            let profile = profile_map.get(agent_id.as_str()).copied();
            let resident = resident_map.get(agent_id.as_str()).copied();
            let launch = launch_map.get(agent_id.as_str()).copied();
            let worker = worker_map.get(agent_id.as_str()).copied();
            let transcript_session_id =
                transcript_session_id_for_agent(&agent_id, &sessions, &primary_actor_id);
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
                "display_name": display_name_for_agent(&agent_id, profile, &primary_actor_id),
                "group_member": agent_id != primary_actor_id,
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
                "launch_id": launch.map(|item| item.launch_id.clone()).unwrap_or_default(),
                "window_title": launch.map(|item| item.window_title.clone()).unwrap_or_default(),
                "log_path": launch.map(|item| item.log_path.clone()).unwrap_or_default(),
                "pid": launch.and_then(|item| item.pid),
                "launch_controls": launch
                    .map(|item| available_launch_controls(item))
                    .unwrap_or_default(),
                "runtime_source": if launch.is_some() {
                    "launch_registry"
                } else if worker.is_some() {
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
            let kind = mailbox_kind(&item.from, &item.to, &item.msg_type, &primary_actor_id);
            json!({
                "msg_id": item.msg_id,
                "from": item.from,
                "to": item.to,
                "type": item.msg_type,
                "kind": kind,
                "direction": mailbox_direction(&item.from, &item.to, &primary_actor_id),
                "message": item.message,
                "ts": item.ts,
                "task_id": item.task_id,
                "trace_id": item.trace_id,
            })
        })
        .collect::<Vec<_>>();

    let main_session_id = transcript_session_id_for_agent(&primary_actor_id, &sessions, &primary_actor_id);
    let main_transcript = main_session_id
        .as_deref()
        .map(|session_id| load_transcript(project, session_id))
        .transpose()?
        .unwrap_or_default();
    let group_members = agents
        .iter()
        .filter_map(|item| item.get("agent_id").cloned())
        .filter(|agent_id| agent_id.as_str() != Some(primary_actor_id.as_str()))
        .collect::<Vec<_>>();
    let launches = model
        .launches
        .iter()
        .map(|launch| {
            let log_tail = if launch.log_path.trim().is_empty() {
                String::new()
            } else {
                crate::launch_log::read_tail(&launch.log_path, 24)
            };
            json!({
                "launch_id": launch.launch_id,
                "agent_id": launch.agent_id,
                "owner": launch.owner,
                "role": launch.role,
                "kind": launch.kind,
                "status": launch.status,
                "pid": launch.pid,
                "process_started_at": launch.process_started_at,
                "task_id": launch.task_id,
                "parent_task_id": launch.parent_task_id,
                "parent_agent_id": launch.parent_agent_id,
                "channel": launch.channel,
                "target": launch.target,
                "window_title": launch.window_title,
                "log_path": launch.log_path,
                "log_tail": log_tail,
                "error": launch.error,
                "controls": available_launch_controls(launch),
            })
        })
        .collect::<Vec<_>>();

    let agent_details = agents
        .iter()
        .filter_map(|item| {
            item.get("agent_id")
                .and_then(Value::as_str)
                .map(|agent_id| (agent_id.to_string(), item.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let process_tree = build_process_tree_payload(
        project,
        model,
        &primary_actor_id,
        &state_map,
        &profile_map,
        &resident_map,
        &launch_map,
        &worker_map,
    )?;

    Ok(json!({
        "launch_actions": [
            {
                "id": "launch.stop",
                "label": "Stop Launch",
                "command": "/launch stop <launch_id>"
            },
            {
                "id": "launch.restart",
                "label": "Restart Launch",
                "command": "/launch restart <launch_id>"
            },
            {
                "id": "launch.logs",
                "label": "View Logs",
                "command": "/launch logs <launch_id> [lines]"
            }
        ],
        "main_friend": {
            "agent_id": primary_actor_id,
            "display_name": display_name_for_agent(&primary_actor_id, profile_map.get(primary_actor_id.as_str()).copied(), &primary_actor_id),
            "chat_kind": "direct",
            "default_target": primary_actor_id,
            "session_id": main_session_id,
            "transcript_message_count": main_transcript.len(),
            "transcript": main_transcript,
        },
        "group_chat": {
            "group_id": "agent-team",
            "title": "Agent Team",
            "chat_kind": "group",
            "primary_actor": primary_actor_id,
            "member_ids": group_members,
            "member_count": agents.iter().filter(|item| item.get("agent_id").and_then(Value::as_str) != Some(primary_actor_id.as_str())).count(),
            "timeline": timeline,
        },
        "launch_actions": [
            {
                "id": "launch.stop",
                "label": "Stop Launch",
                "tool_name": "launch_stop"
            },
            {
                "id": "launch.restart",
                "label": "Restart Launch",
                "tool_name": "launch_restart"
            },
            {
                "id": "launch.logs",
                "label": "View Logs",
                "tool_name": "launch_log_read"
            }
        ],
        "launch_mode": {
            "requested_mode": model.summary.launch_mode,
            "description": model.summary.launch_mode_description,
            "effective_mode": model.summary.launch_effective_mode,
            "backend": model.summary.launch_backend,
            "note": model.summary.launch_backend_note,
        },
        "launches": launches,
        "agents": agents,
        "agent_details": agent_details,
        "process_tree": process_tree,
    }))
}

fn build_process_tree_payload(
    project: &ProjectContext,
    model: &crate::project_tools::SystemModel,
    primary_actor_id: &str,
    state_map: &HashMap<&str, &crate::project_tools::AgentState>,
    profile_map: &HashMap<&str, &crate::project_tools::AgentProfile>,
    resident_map: &HashMap<&str, &crate::project_tools::SystemResident>,
    launch_map: &HashMap<&str, &crate::project_tools::SystemLaunch>,
    worker_map: &HashMap<&str, &crate::team::WorkerEndpoint>,
) -> anyhow::Result<Value> {
    let tasks = project.tasks().list_records()?;
    let mut children_map = BTreeMap::<Option<u64>, Vec<crate::project_tools::TaskRecord>>::new();
    for task in tasks {
        children_map.entry(task.parent_task_id).or_default().push(task);
    }
    for children in children_map.values_mut() {
        children.sort_by_key(|task| task.id);
    }

    let primary_state = state_map.get(primary_actor_id).copied();
    let primary_profile = profile_map.get(primary_actor_id).copied();
    let primary_resident = resident_map.get(primary_actor_id).copied();
    let primary_launch = launch_map.get(primary_actor_id).copied();
    let primary_root = json!({
        "node_id": format!("agent:{primary_actor_id}"),
        "parent_node_id": Value::Null,
        "kind": "root_actor",
        "label": display_name_for_agent(primary_actor_id, primary_profile, primary_actor_id),
        "agent_id": primary_actor_id,
        "task_id": Value::Null,
        "status": primary_state
            .map(|item| item.status.clone())
            .or_else(|| primary_resident.map(|item| item.status.clone()))
            .unwrap_or_else(|| "unknown".to_string()),
        "role": primary_profile
            .map(|item| item.role.clone())
            .or_else(|| primary_resident.map(|item| item.role.clone()))
            .unwrap_or_else(|| "root".to_string()),
        "runtime_source": if primary_launch.is_some() {
            "launch_registry"
        } else if primary_resident.is_some() {
            "resident_runtime"
        } else {
            "agent_registry"
        },
        "channel": primary_state.and_then(|item| item.channel.clone()).unwrap_or_default(),
        "target": primary_state.and_then(|item| item.target.clone()).unwrap_or_default(),
        "note": primary_state.and_then(|item| item.note.clone()).unwrap_or_default(),
        "launch_id": primary_launch.map(|item| item.launch_id.clone()).unwrap_or_default(),
        "window_title": primary_launch.map(|item| item.window_title.clone()).unwrap_or_default(),
        "log_path": primary_launch.map(|item| item.log_path.clone()).unwrap_or_default(),
        "pid": primary_launch.and_then(|item| item.pid),
        "launch_controls": primary_launch
            .map(available_launch_controls)
            .unwrap_or_default(),
        "priority": Value::Null,
        "depth": 0,
        "children": [],
    });

    let mut resident_nodes = model
        .residents
        .iter()
        .filter(|resident| resident.agent_id != primary_actor_id)
        .map(|resident| {
            let state = state_map.get(resident.agent_id.as_str()).copied();
            let launch = launch_map.get(resident.agent_id.as_str()).copied();
            json!({
                "node_id": format!("resident:{}", resident.agent_id),
                "parent_node_id": format!("agent:{primary_actor_id}"),
                "kind": "resident",
                "label": display_name_for_agent(
                    &resident.agent_id,
                    profile_map.get(resident.agent_id.as_str()).copied(),
                    primary_actor_id,
                ),
                "agent_id": resident.agent_id,
                "task_id": state.and_then(|item| item.current_task_id),
                "status": state
                    .map(|item| item.status.clone())
                    .unwrap_or_else(|| resident.status.clone()),
                "role": resident.role,
                "runtime_source": if launch.is_some() { "launch_registry" } else { "resident_runtime" },
                "channel": launch
                    .map(|item| item.channel.clone())
                    .or_else(|| state.and_then(|item| item.channel.clone()))
                    .unwrap_or_else(|| "resident".to_string()),
                "target": launch
                    .map(|item| item.target.clone())
                    .or_else(|| state.and_then(|item| item.target.clone()))
                    .unwrap_or_else(|| resident.behavior_mode.clone()),
                "note": state.and_then(|item| item.note.clone()).unwrap_or_else(|| resident.note.clone()),
                "launch_id": launch.map(|item| item.launch_id.clone()).unwrap_or_default(),
                "window_title": launch.map(|item| item.window_title.clone()).unwrap_or_default(),
                "log_path": launch.map(|item| item.log_path.clone()).unwrap_or_default(),
                "pid": launch.and_then(|item| item.pid),
                "launch_controls": launch
                    .map(available_launch_controls)
                    .unwrap_or_default(),
                "priority": Value::Null,
                "depth": 1,
                "children": [],
            })
        })
        .collect::<Vec<_>>();
    resident_nodes.sort_by(|a, b| {
        a.get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get("label").and_then(Value::as_str).unwrap_or_default())
    });

    let mut flat_nodes = vec![primary_root.clone()];
    flat_nodes.extend(resident_nodes.iter().cloned());

    let mut roots = resident_nodes;
    if let Some(top_level_tasks) = children_map.get(&None) {
        for task in top_level_tasks {
            let node = build_task_tree_node(
                task,
                Some(format!("agent:{primary_actor_id}")),
                &children_map,
                state_map,
                profile_map,
                resident_map,
                launch_map,
                worker_map,
                primary_actor_id,
                &mut flat_nodes,
            );
            roots.push(node);
        }
    }

    let mut root_node = primary_root;
    if let Some(object) = root_node.as_object_mut() {
        object.insert("children".to_string(), Value::Array(roots.clone()));
    }

    Ok(json!({
        "root_actor_id": primary_actor_id,
        "roots": [root_node],
        "nodes": flat_nodes,
    }))
}

#[allow(clippy::too_many_arguments)]
fn build_task_tree_node(
    task: &crate::project_tools::TaskRecord,
    parent_node_id: Option<String>,
    children_map: &BTreeMap<Option<u64>, Vec<crate::project_tools::TaskRecord>>,
    state_map: &HashMap<&str, &crate::project_tools::AgentState>,
    profile_map: &HashMap<&str, &crate::project_tools::AgentProfile>,
    resident_map: &HashMap<&str, &crate::project_tools::SystemResident>,
    launch_map: &HashMap<&str, &crate::project_tools::SystemLaunch>,
    worker_map: &HashMap<&str, &crate::team::WorkerEndpoint>,
    primary_actor_id: &str,
    flat_nodes: &mut Vec<Value>,
) -> Value {
    let owner = task.owner.trim();
    let state = (!owner.is_empty())
        .then(|| state_map.get(owner).copied())
        .flatten();
    let worker = (!owner.is_empty())
        .then(|| worker_map.get(owner).copied())
        .flatten();
    let resident = (!owner.is_empty())
        .then(|| resident_map.get(owner).copied())
        .flatten();
    let launch = (!owner.is_empty())
        .then(|| launch_map.get(owner).copied())
        .flatten();
    let profile = (!owner.is_empty())
        .then(|| profile_map.get(owner).copied())
        .flatten();
    let label = if owner.is_empty() {
        format!("#{} {}", task.id, task.subject)
    } else {
        format!(
            "#{} {} [{}]",
            task.id,
            task.subject,
            display_name_for_agent(owner, profile, primary_actor_id)
        )
    };

    let child_nodes = children_map
        .get(&Some(task.id))
        .map(|children| {
            children
                .iter()
                .map(|child| {
                    build_task_tree_node(
                        child,
                        Some(format!("task:{}", task.id)),
                        children_map,
                        state_map,
                        profile_map,
                        resident_map,
                        launch_map,
                        worker_map,
                        primary_actor_id,
                        flat_nodes,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let node = json!({
        "node_id": format!("task:{}", task.id),
        "parent_node_id": parent_node_id,
        "kind": if worker.is_some() { "worker_task" } else { "task" },
        "label": label,
        "agent_id": if owner.is_empty() { Value::Null } else { Value::String(owner.to_string()) },
        "task_id": task.id,
        "status": task.status,
        "role": task.role_hint,
        "runtime_source": if launch.is_some() {
            "launch_registry"
        } else if worker.is_some() {
            "worker_endpoint"
        } else if resident.is_some() {
            "resident_runtime"
        } else if state.is_some() {
            "agent_registry"
        } else {
            "task_registry"
        },
        "channel": launch
            .map(|item| item.channel.clone())
            .or_else(|| worker
            .map(|item| item.channel.clone())
            )
            .or_else(|| state.and_then(|item| item.channel.clone()))
            .unwrap_or_default(),
        "target": launch
            .map(|item| item.target.clone())
            .or_else(|| worker
            .map(|item| item.target.clone())
            )
            .or_else(|| state.and_then(|item| item.target.clone()))
            .unwrap_or_default(),
        "note": state.and_then(|item| item.note.clone()).unwrap_or_default(),
        "launch_id": launch.map(|item| item.launch_id.clone()).unwrap_or_default(),
        "window_title": launch.map(|item| item.window_title.clone()).unwrap_or_default(),
        "log_path": launch.map(|item| item.log_path.clone()).unwrap_or_default(),
        "pid": launch.and_then(|item| item.pid),
        "launch_controls": launch
            .map(available_launch_controls)
            .unwrap_or_default(),
        "priority": task.priority,
        "depth": task.depth,
        "subject": task.subject,
        "owner": task.owner,
        "children": child_nodes,
    });
    flat_nodes.push(node.clone());
    node
}

fn transcript_session_id_for_agent(
    agent_id: &str,
    sessions: &[crate::project_tools::SessionRecord],
    primary_actor_id: &str,
) -> Option<String> {
    if agent_id == primary_actor_id {
        if sessions.iter().any(|item| item.session_id == "cli-main") {
            return Some("cli-main".to_string());
        }
        return sessions
            .iter()
            .find(|item| item.focus == agent_id)
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
    primary_actor_id: &str,
) -> String {
    if agent_id == primary_actor_id {
        return "Main".to_string();
    }
    profile
        .map(|item| item.role.clone())
        .filter(|item| !item.is_empty())
        .map(|role| format!("{agent_id} ({role})"))
        .unwrap_or_else(|| agent_id.to_string())
}

fn mailbox_direction(from: &str, to: &str, primary_actor_id: &str) -> &'static str {
    if from == primary_actor_id {
        "main_to_agent"
    } else if to == primary_actor_id {
        "agent_to_main"
    } else {
        "agent_to_agent"
    }
}

fn mailbox_kind(from: &str, to: &str, msg_type: &str, primary_actor_id: &str) -> &'static str {
    if msg_type.starts_with("task.") {
        "task_update"
    } else if from == primary_actor_id || to == primary_actor_id {
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

fn available_launch_controls(launch: &crate::project_tools::SystemLaunch) -> Vec<String> {
    match launch.status.as_str() {
        "requested" | "running" => vec!["stop".to_string(), "restart".to_string()],
        "failed" | "stopped" => vec!["restart".to_string()],
        _ => Vec::new(),
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
        && !is_bootstrap_shell(&page.html)
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

fn is_bootstrap_shell(html: &str) -> bool {
    html.contains("waiting for generated ui page...")
        || html.contains("The real interface is generated by the UI agent.")
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

fn normalize_primary_actor_id(preferred_actor_id: &str, agent_ids: &BTreeSet<String>) -> String {
    let preferred = preferred_actor_id.trim();
    if !preferred.is_empty() && agent_ids.contains(preferred) {
        return preferred.to_string();
    }
    if agent_ids.contains(preferred) {
        return preferred.to_string();
    }
    if agent_ids.contains("lead") {
        return "lead".to_string();
    }
    agent_ids
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(|| "lead".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_process_tree_payload, is_bootstrap_shell, mailbox_direction, mailbox_kind,
        normalize_primary_actor_id, transcript_session_id_for_agent,
    };
    use crate::project_tools::{ProjectContext, SessionRecord, TaskCreateOptions};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!("{}_{}_{}", name, std::process::id(), now_nanos());
            let path = std::env::temp_dir().join("tests").join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn now_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    }

    fn project_context(repo_root: &Path) -> ProjectContext {
        ProjectContext::new(repo_root.to_path_buf()).expect("project context")
    }

    #[test]
    fn primary_actor_prefers_available_non_lead_node() {
        let mut agent_ids = BTreeSet::new();
        agent_ids.insert("teammate-root".to_string());
        agent_ids.insert("reviewer".to_string());
        assert_eq!(
            normalize_primary_actor_id("teammate-root", &agent_ids),
            "teammate-root"
        );
    }

    #[test]
    fn transcript_lookup_uses_cli_main_for_primary_actor() {
        let sessions = vec![
            SessionRecord {
                session_id: "cli-main".to_string(),
                label: Some("primary".to_string()),
                focus: "worker(7)".to_string(),
                status: "active".to_string(),
                updated_at: 1,
            },
            SessionRecord {
                session_id: "teammate-root".to_string(),
                label: None,
                focus: "lead".to_string(),
                status: "active".to_string(),
                updated_at: 2,
            },
        ];
        assert_eq!(
            transcript_session_id_for_agent("teammate-root", &sessions, "teammate-root"),
            Some("cli-main".to_string())
        );
    }

    #[test]
    fn mailbox_semantics_follow_primary_actor() {
        assert_eq!(
            mailbox_direction("teammate-root", "reviewer", "teammate-root"),
            "main_to_agent"
        );
        assert_eq!(
            mailbox_direction("reviewer", "teammate-root", "teammate-root"),
            "agent_to_main"
        );
        assert_eq!(
            mailbox_kind("reviewer", "teammate-root", "message", "teammate-root"),
            "chat"
        );
    }

    #[test]
    fn bootstrap_shell_detection_matches_placeholder_html() {
        assert!(is_bootstrap_shell(
            "<html><body>waiting for generated ui page...</body></html>"
        ));
        assert!(!is_bootstrap_shell("<html><body><div id=\"app\">ready</div></body></html>"));
    }

    #[test]
    fn process_tree_nests_child_tasks_under_parent() {
        let temp = TestDir::new("ui_process_tree");
        let project = project_context(temp.path());
        project
            .agents()
            .ensure_profile("root-node", "root", "coordinate work", &["route"], &["block"])
            .expect("root profile");
        project
            .agents()
            .set_state(
                "root-node",
                "running",
                None,
                Some("resident"),
                Some("root-loop"),
                Some("coordinating"),
            )
            .expect("root state");

        let parent = project
            .tasks()
            .create_detailed("parent", "coordinate", TaskCreateOptions::default())
            .expect("parent");
        let parent: crate::project_tools::TaskRecord =
            serde_json::from_str(&parent).expect("parse parent");
        project
            .tasks()
            .update(parent.id, Some("in_progress"), Some("teammate-1"), None)
            .expect("parent update");
        project
            .agents()
            .ensure_profile(
                "teammate-1",
                "developer",
                "do parent task",
                &["execute"],
                &["wander"],
            )
            .expect("worker profile");
        project
            .agents()
            .set_state(
                "teammate-1",
                "running",
                Some(parent.id),
                Some("terminal"),
                Some("term-1"),
                Some("working"),
            )
            .expect("worker state");

        let child = project
            .tasks()
            .create_detailed(
                "child",
                "nested work",
                TaskCreateOptions {
                    parent_task_id: Some(parent.id),
                    depth: Some(1),
                    ..TaskCreateOptions::default()
                },
            )
            .expect("child");
        let child: crate::project_tools::TaskRecord =
            serde_json::from_str(&child).expect("parse child");
        project
            .tasks()
            .update(child.id, Some("pending"), Some("teammate-2"), None)
            .expect("child update");

        let model = project.system_model().rebuild(&project).expect("model");
        let states = project.agents().states().expect("states");
        let profiles = project.agents().profiles().expect("profiles");
        let state_map = states
            .iter()
            .map(|item| (item.agent_id.as_str(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let profile_map = profiles
            .iter()
            .map(|item| (item.agent_id.as_str(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let resident_map = model
            .residents
            .iter()
            .map(|item| (item.agent_id.as_str(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let launch_map = model
            .launches
            .iter()
            .map(|item| (item.agent_id.as_str(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let worker_map = std::collections::HashMap::new();

        let tree = build_process_tree_payload(
            &project,
            &model,
            "root-node",
            &state_map,
            &profile_map,
            &resident_map,
            &launch_map,
            &worker_map,
        )
        .expect("process tree");

        let roots = tree["roots"][0]["children"]
            .as_array()
            .expect("root children");
        let parent_node = roots
            .iter()
            .find(|item| item["node_id"].as_str() == Some(&format!("task:{}", parent.id)))
            .expect("parent node");
        let child_nodes = parent_node["children"].as_array().expect("child nodes");
        let child_node_id = format!("task:{}", child.id);
        let parent_node_id = format!("task:{}", parent.id);
        assert_eq!(
            child_nodes[0]["node_id"].as_str(),
            Some(child_node_id.as_str())
        );
        assert_eq!(
            child_nodes[0]["parent_node_id"].as_str(),
            Some(parent_node_id.as_str())
        );
    }
}
