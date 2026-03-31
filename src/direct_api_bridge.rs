use std::sync::OnceLock;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::wire::{WireFrame, WireRequest};

#[derive(Debug)]
pub struct DirectApiRequest {
    pub request: WireRequest,
    pub frames_tx: UnboundedSender<WireFrame>,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub session_label: Option<String>,
}

static ROOT_API_BRIDGE: OnceLock<UnboundedSender<DirectApiRequest>> = OnceLock::new();

pub fn install_root_api_bridge(sender: UnboundedSender<DirectApiRequest>) {
    let _ = ROOT_API_BRIDGE.set(sender);
}

pub fn root_api_bridge() -> Option<UnboundedSender<DirectApiRequest>> {
    ROOT_API_BRIDGE.get().cloned()
}

pub fn direct_api_channel() -> (
    UnboundedSender<DirectApiRequest>,
    UnboundedReceiver<DirectApiRequest>,
) {
    unbounded_channel()
}
