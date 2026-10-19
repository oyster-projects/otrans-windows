use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub lan_server_url: String,
    pub wan_server_url: String,
    pub server_id: String,
    pub device_name: String,
    pub inbox_path: String,
    pub outbox_path: String,
    pub start_at_login: bool,
    #[serde(default)]
    pub lan_peer_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    pub active_server_url: String,
    pub active_route: String,
    pub realtime_status: String,
    pub lan_peer_status: String,
    pub lan_peer_port: u16,
    pub lan_peer_detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub kind: String,
    pub message: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedTransfer {
    pub folder: String,
    #[serde(default)]
    pub route: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFile {
    pub signature: String,
    pub transfer_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSource {
    pub path: String,
    pub size: u64,
    pub signature: String,
    pub name: String,
    pub mime_type: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub cursor: i64,
    pub device_id: String,
    pub transfers: HashMap<String, ManagedTransfer>,
    pub sent_files: HashMap<String, SentFile>,
    pub peer_sources: HashMap<String, PeerSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedState {
    pub status: String,
    pub settings: Settings,
    pub sync: SyncState,
    pub connection: ConnectionState,
    pub activity: Vec<Activity>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicState {
    pub settings: Settings,
    pub paired: bool,
    pub device_id: String,
    pub status: String,
    pub syncing: bool,
    pub connection: ConnectionState,
    pub lan_peer_source_count: usize,
    pub activity: Vec<Activity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub server_id: String,
    pub protocol_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub sync_mode: String,
    pub cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    pub device: Device,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferItem {
    pub id: Option<String>,
    pub kind: String,
    pub text: Option<String>,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    pub source_device_id: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub committed_at: Option<String>,
    pub items: Vec<TransferItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTransferResponse {
    pub transfer: Transfer,
    pub missing_blobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPage {
    pub events: Vec<TransferEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferEvent {
    pub sequence: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub transfer_id: String,
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsTicket {
    pub ticket: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSourcesResponse {
    pub sources: Vec<PeerAdvertisement>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerAdvertisement {
    pub device_id: String,
    pub device_name: String,
    pub addresses: Vec<String>,
    pub ticket: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsInput {
    pub lan_server_url: String,
    pub wan_server_url: String,
    pub device_name: String,
    pub inbox_path: String,
    pub outbox_path: String,
    pub start_at_login: bool,
    pub lan_peer_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInput {
    pub lan_server_url: String,
    pub wan_server_url: String,
    pub device_name: String,
    pub pair_code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendDraftInput {
    pub text: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardContent {
    pub kind: String,
    pub text: String,
    pub paths: Vec<String>,
}
