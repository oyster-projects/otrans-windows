use std::{collections::HashSet, path::{Path, PathBuf}, sync::Arc, time::Duration};

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;

use crate::{
    endpoint,
    models::{ManagedTransfer, PeerSource, PublicState, Transfer, TransferItem},
    state::StateStore,
};

#[derive(Clone)]
pub struct AppRuntime {
    pub store: Arc<Mutex<StateStore>>,
    pub client: reqwest::Client,
    realtime: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
}

impl AppRuntime {
    pub fn new(store: StateStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            client: reqwest::Client::builder().user_agent("OTrans-Rust/0.1").build().expect("HTTP client"),
            realtime: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn public_state(&self) -> PublicState {
        self.store.lock().await.public()
    }

    pub async fn emit_state(&self, app: &AppHandle) {
        let _ = app.emit("state-changed", self.public_state().await);
    }

    pub async fn restart_realtime(&self, app: AppHandle) {
        if let Some(task) = self.realtime.lock().await.take() { task.abort(); }
        let paired = self.store.lock().await.public().paired;
        if !paired { return; }
        let runtime = self.clone();
        *self.realtime.lock().await = Some(tauri::async_runtime::spawn(async move {
            runtime.realtime_loop(app).await;
        }));
    }

    pub async fn stop_realtime(&self) {
        if let Some(task) = self.realtime.lock().await.take() { task.abort(); }
    }

    async fn realtime_loop(&self, app: AppHandle) {
        let mut attempt = 0u32;
        loop {
            self.set_realtime(&app, if attempt == 0 { "connecting" } else { "reconnecting" }).await;
            let connected = async {
                let endpoint = endpoint::resolve(&self.store, &self.client, true).await?;
                let ticket = endpoint.api.websocket_ticket().await?;
                let url = endpoint.api.websocket_url(&ticket.ticket)?;
                let (mut socket, _) = connect_async(url.as_str()).await?;
                self.set_realtime(&app, "connected").await;
                attempt = 0;
                let _ = self.sync_once(&app, false).await;
                loop {
                    match tokio::time::timeout(Duration::from_secs(60), socket.next()).await {
                        Ok(Some(Ok(message))) => {
                            if message.is_text() || message.is_binary() {
                                let _ = self.sync_once(&app, false).await;
                            }
                            if message.is_close() { break; }
                        }
                        Ok(Some(Err(error))) => return Err(anyhow!(error)),
                        Ok(None) => break,
                        Err(_) => {
                            let _ = self.sync_once(&app, false).await;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            }.await;
            if connected.is_err() {
                self.set_realtime(&app, "reconnecting").await;
            }
            attempt = attempt.saturating_add(1);
            let delay = 2u64.saturating_pow(attempt.min(5)).min(30);
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
    }

    async fn set_realtime(&self, app: &AppHandle, status: &str) {
        let mut store = self.store.lock().await;
        if store.state.connection.realtime_status != status {
            store.state.connection.realtime_status = status.into();
            let _ = store.save();
        }
        drop(store);
        self.emit_state(app).await;
    }

    pub async fn sync_once(&self, app: &AppHandle, manual: bool) -> Result<()> {
        {
            let mut store = self.store.lock().await;
            if store.syncing { return Ok(()); }
            store.syncing = true;
            store.state.status = "正在同步…".into();
        }
        self.emit_state(app).await;
        let result = self.sync_pass(app).await;
        let mut store = self.store.lock().await;
        store.syncing = false;
        match &result {
            Ok((received, deleted)) => {
                let summary = if *received > 0 || *deleted > 0 {
                    format!("同步完成：接收 {received} 条，删除 {deleted} 条")
                } else { "已是最新状态".into() };
                store.state.status.clone_from(&summary);
                if manual || *received > 0 || *deleted > 0 { store.add_activity("success", summary); }
            }
            Err(error) => {
                store.state.status = format!("同步失败：{error}");
                store.add_activity("error", format!("同步失败：{error}"));
            }
        }
        let _ = store.save();
        drop(store);
        self.emit_state(app).await;
        result.map(|_| ())
    }

    async fn sync_pass(&self, app: &AppHandle) -> Result<(usize, usize)> {
        let endpoint = endpoint::resolve(&self.store, &self.client, true).await?;
        let device = endpoint.api.device().await?;
        {
            let mut store = self.store.lock().await;
            store.state.sync.device_id.clone_from(&device.id);
        }
        let mut cursor = {
            let store = self.store.lock().await;
            store.state.sync.cursor.max(device.cursor)
        };
        let mut received = 0usize;
        let mut deleted = 0usize;
        loop {
            let page = endpoint.api.events(cursor).await?;
            if page.events.is_empty() { break; }
            for event in page.events {
                if event.event_type == "transfer.created" {
                    if let Some(payload) = event.payload {
                        let transfer: Transfer = serde_json::from_value(payload)?;
                        if transfer.source_device_id != device.id && self.receive_transfer(app, &endpoint.api, &transfer).await? {
                            received += 1;
                        }
                    }
                } else if event.event_type == "transfer.deleted" && self.delete_transfer(&event.transfer_id).await? {
                    deleted += 1;
                }
                cursor = cursor.max(event.sequence);
            }
            endpoint.api.update_cursor(cursor).await?;
            let mut store = self.store.lock().await;
            store.state.sync.cursor = cursor;
            store.save()?;
        }
        Ok((received, deleted))
    }

    async fn receive_transfer(&self, app: &AppHandle, api: &crate::api::OTransApi, transfer: &Transfer) -> Result<bool> {
        let inbox = {
            let store = self.store.lock().await;
            if store.state.sync.transfers.contains_key(&transfer.id) { return Ok(false); }
            PathBuf::from(&store.state.settings.inbox_path)
        };
        tokio::fs::create_dir_all(&inbox).await?;
        let folder = unique_folder(&inbox, &transfer_folder_name(transfer)).await?;
        let temporary = inbox.join(format!(".partial-{}", transfer.id));
        let _ = tokio::fs::remove_dir_all(&temporary).await;
        tokio::fs::create_dir_all(&temporary).await?;
        let result = async {
            let mut text_index = 0usize;
            let mut used = HashSet::new();
            let mut received_files = Vec::new();
            for item in &transfer.items {
                if item.kind == "text" {
                    text_index += 1;
                    let name = unique_name(&format!("文本-{text_index}.txt"), &mut used);
                    tokio::fs::write(temporary.join(name), item.text.as_deref().unwrap_or("")).await?;
                } else if item.kind == "file" {
                    let name = unique_name(&sanitize_file_name(item.name.as_deref().unwrap_or("未命名文件")), &mut used);
                    self.download_file(api, item, &temporary.join(&name)).await?;
                    received_files.push((item.clone(), name));
                }
            }
            tokio::fs::write(temporary.join(".otrans-transfer.json"), serde_json::to_vec_pretty(transfer)?).await?;
            tokio::fs::rename(&temporary, &folder).await?;
            let mut store = self.store.lock().await;
            for (item, name) in received_files {
                if let Some(digest) = item.sha256 {
                    let path = folder.join(&name);
                    let metadata = tokio::fs::metadata(&path).await?;
                    store.state.sync.peer_sources.insert(digest, PeerSource {
                        path: path.to_string_lossy().into_owned(), size: metadata.len(), signature: file_signature(&metadata),
                        name, mime_type: item.mime_type.unwrap_or_else(|| "application/octet-stream".into()),
                        updated_at: chrono::Utc::now().timestamp_millis(),
                    });
                }
            }
            store.state.sync.transfers.insert(transfer.id.clone(), ManagedTransfer { folder: folder.to_string_lossy().into_owned() });
            let title = transfer.title.as_deref().unwrap_or("未命名中转内容");
            store.add_activity("received", format!("收到：{title}"));
            let _ = app.notification().builder().title("OTrans 收到新内容").body(title).show();
            Ok::<(), anyhow::Error>(())
        }.await;
        if result.is_err() { let _ = tokio::fs::remove_dir_all(&temporary).await; }
        result?;
        Ok(true)
    }

    async fn download_file(&self, api: &crate::api::OTransApi, item: &TransferItem, destination: &Path) -> Result<()> {
        let digest = item.sha256.as_deref().ok_or_else(|| anyhow!("文件记录缺少 SHA-256"))?;
        let name = item.name.as_deref().unwrap_or("file");
        let bytes = api.get_blob(digest, name).await?.bytes().await?;
        if item.size.is_some_and(|expected| expected != bytes.len() as u64) {
            return Err(anyhow!("{name} 文件大小校验失败"));
        }
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != digest.to_lowercase() { return Err(anyhow!("{name} 完整性校验失败")); }
        tokio::fs::write(destination, bytes).await?;
        Ok(())
    }

    async fn delete_transfer(&self, id: &str) -> Result<bool> {
        let (inbox, folder) = {
            let store = self.store.lock().await;
            let Some(record) = store.state.sync.transfers.get(id) else { return Ok(false); };
            (PathBuf::from(&store.state.settings.inbox_path), PathBuf::from(&record.folder))
        };
        if !is_inside(&inbox, &folder) { return Err(anyhow!("拒绝删除 Inbox 之外的路径")); }
        let _ = tokio::fs::remove_dir_all(&folder).await;
        let mut store = self.store.lock().await;
        store.state.sync.transfers.remove(id);
        store.add_activity("deleted", format!("已同步删除：{}", folder.file_name().and_then(|v| v.to_str()).unwrap_or("中转内容")));
        Ok(true)
    }
}

fn sanitize_file_name(value: &str) -> String {
    let mut name: String = value.trim().chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    while name.ends_with(['.', ' ']) { name.pop(); }
    if name.is_empty() || name == "." || name == ".." { name = "未命名文件".into(); }
    let upper = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" |
        "COM6" | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9") {
        name.insert(0, '_');
    }
    name.chars().take(180).collect()
}

fn unique_name(candidate: &str, used: &mut HashSet<String>) -> String {
    let path = Path::new(candidate);
    let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("文件");
    let extension = path.extension().and_then(|v| v.to_str()).map(|v| format!(".{v}")).unwrap_or_default();
    let mut result = candidate.to_string();
    let mut suffix = 2;
    while !used.insert(result.to_lowercase()) {
        result = format!("{stem} ({suffix}){extension}"); suffix += 1;
    }
    result
}

fn transfer_folder_name(transfer: &Transfer) -> String {
    let timestamp = transfer.committed_at.as_deref().unwrap_or(&transfer.created_at);
    let stamp = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.with_timezone(&chrono::Local).format("%Y%m%d-%H%M%S").to_string())
        .unwrap_or_else(|_| "unknown-time".into());
    let files = transfer.items.iter().filter(|item| item.kind == "file").collect::<Vec<_>>();
    let label = if let Some(first) = files.first() {
        let name = sanitize_file_name(first.name.as_deref().unwrap_or("文件"));
        let stem = Path::new(&name).file_stem().and_then(|value| value.to_str()).unwrap_or("文件");
        let mut label = screenshot_label(stem).unwrap_or_else(|| {
            let prefix = if first.mime_type.as_deref().unwrap_or("").starts_with("image/") { "图片 " } else { "" };
            format!("{prefix}{stem}")
        });
        if transfer.items.len() > 1 { label.push_str(&format!(" 等{}项", transfer.items.len())); }
        label
    } else if transfer.items.iter().any(|item| item.kind == "text") {
        "文本".into()
    } else {
        let title = transfer.title.as_deref().unwrap_or("中转内容");
        if title.starts_with("来自") {
            title.split_once(['：', ':']).map(|(_, value)| value).unwrap_or("中转内容").into()
        } else { title.into() }
    };
    let concise: String = sanitize_file_name(&label).chars().take(42).collect();
    format!("{stamp} {concise}")
}

fn screenshot_label(stem: &str) -> Option<String> {
    let lower = stem.to_ascii_lowercase();
    let remainder = ["screenshot", "screen_capture", "screencapture", "截图"].iter()
        .find_map(|prefix| lower.strip_prefix(prefix)).map(|value| value.trim_start_matches(['_', '-', ' ']))?;
    let bytes = remainder.as_bytes();
    if bytes.len() < 15 || !bytes[..8].iter().all(u8::is_ascii_digit) || !matches!(bytes[8], b'_' | b'-')
        || !bytes[9..15].iter().all(u8::is_ascii_digit) { return None; }
    Some(format!("截图 {}-{}", &remainder[..8], &remainder[9..15]))
}

async fn unique_folder(inbox: &Path, base_name: &str) -> Result<PathBuf> {
    for suffix in 1..1000 {
        let name = if suffix == 1 { base_name.to_string() } else { format!("{base_name} ({suffix})") };
        let candidate = inbox.join(name);
        match tokio::fs::metadata(&candidate).await {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("无法为接收内容分配目录名称"))
}

fn is_inside(root: &Path, candidate: &Path) -> bool {
    candidate != root && candidate.strip_prefix(root).is_ok()
}

fn file_signature(metadata: &std::fs::Metadata) -> String {
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis()).unwrap_or(0);
    format!("{}:{modified}", metadata.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concise_screenshot_folder_drops_device_prefix_and_id() {
        let transfer = Transfer {
            id: "af547b85-rest".into(), source_device_id: "phone".into(),
            title: Some("来自 HarmonyOS：screenshot_20261017_124314_com.example".into()),
            status: "committed".into(), created_at: "2026-10-17T04:43:42Z".into(),
            committed_at: Some("2026-10-17T04:43:42Z".into()),
            items: vec![TransferItem { id: None, kind: "file".into(), text: None,
                name: Some("screenshot_20261017_124314_com.example.png".into()), mime_type: Some("image/png".into()),
                size: Some(1), sha256: Some("0".repeat(64)) }],
        };
        assert_eq!(transfer_folder_name(&transfer), "20261017-124342 截图 20261017-124314");
    }
}
