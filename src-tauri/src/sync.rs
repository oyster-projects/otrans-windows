use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::Mutex};
use tokio_tungstenite::connect_async;

use crate::{
    endpoint, lan_peer,
    models::{ManagedTransfer, PeerSource, PublicState, SentFile, Transfer, TransferItem},
    outbound,
    state::StateStore,
};

struct DownloadOutcome {
    direct_source: Option<String>,
    direct_failure: Option<String>,
}

#[derive(Clone)]
pub struct AppRuntime {
    pub store: Arc<Mutex<StateStore>>,
    pub client: reqwest::Client,
    peer_client: reqwest::Client,
    realtime: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
    peer: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
    maintenance: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
    outbox_lock: Arc<Mutex<()>>,
}

impl AppRuntime {
    pub fn new(store: StateStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            client: reqwest::Client::builder()
                .user_agent("OTrans-Rust/0.2")
                .build()
                .expect("HTTP client"),
            peer_client: reqwest::Client::builder()
                .user_agent("OTrans-Rust/0.2 LAN peer")
                .no_proxy()
                .connect_timeout(Duration::from_millis(1800))
                .timeout(Duration::from_secs(180))
                .build()
                .expect("peer HTTP client"),
            realtime: Arc::new(Mutex::new(None)),
            peer: Arc::new(Mutex::new(None)),
            maintenance: Arc::new(Mutex::new(None)),
            outbox_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn public_state(&self) -> PublicState {
        self.store.lock().await.public()
    }

    pub async fn emit_state(&self, app: &AppHandle) {
        let _ = app.emit("state-changed", self.public_state().await);
    }

    pub async fn restart_services(&self, app: AppHandle) {
        self.restart_realtime(app.clone()).await;
        self.restart_peer(app.clone()).await;
        self.restart_maintenance(app).await;
    }

    pub async fn stop_services(&self) {
        self.stop_realtime().await;
        if let Some(task) = self.peer.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.maintenance.lock().await.take() {
            task.abort();
        }
    }

    async fn restart_peer(&self, app: AppHandle) {
        if let Some(task) = self.peer.lock().await.take() {
            task.abort();
        }
        let (enabled, paired) = {
            let store = self.store.lock().await;
            (store.state.settings.lan_peer_enabled, store.public().paired)
        };
        if !enabled || !paired {
            self.set_peer_state(&app, "disabled", 0, "局域网直传未启用")
                .await;
            return;
        }
        self.set_peer_state(&app, "starting", 0, "正在启动局域网直传…")
            .await;
        match TcpListener::bind("0.0.0.0:0").await {
            Ok(listener) => {
                let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
                self.set_peer_state(
                    &app,
                    "ready",
                    port,
                    &format!("动态端口 {port}，等待局域网设备"),
                )
                .await;
                let runtime = self.clone();
                *self.peer.lock().await = Some(tauri::async_runtime::spawn(async move {
                    lan_peer::run(listener, runtime, app).await;
                }));
            }
            Err(error) => {
                self.set_peer_state(&app, "error", 0, &format!("无法监听局域网端口：{error}"))
                    .await;
            }
        }
    }

    async fn restart_maintenance(&self, app: AppHandle) {
        if let Some(task) = self.maintenance.lock().await.take() {
            task.abort();
        }
        let paired = self.store.lock().await.public().paired;
        if !paired {
            return;
        }
        let runtime = self.clone();
        *self.maintenance.lock().await = Some(tauri::async_runtime::spawn(async move {
            runtime.maintenance_loop(app).await;
        }));
    }

    async fn maintenance_loop(&self, app: AppHandle) {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_tick = Instant::now();
        let mut last_route_check = Instant::now();
        let mut last_safety_sync = Instant::now();
        loop {
            tick.tick().await;
            let elapsed = last_tick.elapsed();
            last_tick = Instant::now();
            if elapsed > Duration::from_secs(15) {
                {
                    let mut store = self.store.lock().await;
                    store.add_activity("info", "检测到系统恢复，正在重连服务");
                }
                self.restart_realtime(app.clone()).await;
                let port = self.store.lock().await.state.connection.lan_peer_port;
                if port > 0 {
                    self.announce_peer(&app, port).await;
                }
            }
            if let Err(error) = self.scan_outbox(&app).await {
                let mut store = self.store.lock().await;
                let message = format!("Outbox 自动发送失败：{error}");
                if store.state.status != message {
                    store.state.status.clone_from(&message);
                    store.add_activity("error", message);
                }
            }
            if last_route_check.elapsed() >= Duration::from_secs(60) {
                last_route_check = Instant::now();
                let before = self
                    .store
                    .lock()
                    .await
                    .state
                    .connection
                    .active_server_url
                    .clone();
                let changed = endpoint::resolve(&self.store, &self.client, true)
                    .await
                    .is_ok_and(|resolved| resolved.api.server_url != before);
                if changed {
                    self.restart_realtime(app.clone()).await;
                }
            }
            if last_safety_sync.elapsed() >= Duration::from_secs(600) {
                last_safety_sync = Instant::now();
                let _ = self.sync_once(&app, false).await;
            }
        }
    }

    pub async fn set_peer_state(&self, app: &AppHandle, status: &str, port: u16, detail: &str) {
        let mut store = self.store.lock().await;
        store.state.connection.lan_peer_status = status.into();
        store.state.connection.lan_peer_port = port;
        store.state.connection.lan_peer_detail = detail.into();
        let _ = store.save();
        drop(store);
        self.emit_state(app).await;
    }

    pub async fn remove_peer_source(&self, digest: &str) {
        let mut store = self.store.lock().await;
        if store.state.sync.peer_sources.remove(digest).is_some() {
            let _ = store.save();
        }
    }

    pub async fn announce_peer(&self, app: &AppHandle, port: u16) {
        let addresses = lan_peer::private_ipv4_addresses(port);
        if addresses.is_empty() {
            self.set_peer_state(app, "no-address", port, "没有可登记的私网 IPv4 地址")
                .await;
            return;
        }
        let sources = self.valid_peer_sources().await;
        let digests = sources.keys().take(512).cloned().collect::<Vec<_>>();
        let result = match endpoint::resolve(&self.store, &self.client, true).await {
            Ok(endpoint) => endpoint.api.announce_peer(&addresses, &digests).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.set_peer_state(
                    app,
                    "ready",
                    port,
                    &format!("已登记 {} 个文件，动态端口 {port}", digests.len()),
                )
                .await;
            }
            Err(error) => {
                self.set_peer_state(app, "offline", port, &format!("等待飞牛登记：{error}"))
                    .await;
            }
        }
    }

    async fn valid_peer_sources(&self) -> HashMap<String, PeerSource> {
        let current = self.store.lock().await.state.sync.peer_sources.clone();
        let mut valid = HashMap::new();
        for (digest, source) in current {
            let Ok(metadata) = tokio::fs::metadata(&source.path).await else {
                continue;
            };
            if metadata.is_file()
                && metadata.len() == source.size
                && lan_peer::metadata_signature(&metadata) == source.signature
            {
                valid.insert(digest, source);
            }
        }
        let mut store = self.store.lock().await;
        if store.state.sync.peer_sources.len() != valid.len() {
            store.state.sync.peer_sources.clone_from(&valid);
            let _ = store.save();
        }
        valid
    }

    pub async fn restart_realtime(&self, app: AppHandle) {
        if let Some(task) = self.realtime.lock().await.take() {
            task.abort();
        }
        let paired = self.store.lock().await.public().paired;
        if !paired {
            return;
        }
        let runtime = self.clone();
        *self.realtime.lock().await = Some(tauri::async_runtime::spawn(async move {
            runtime.realtime_loop(app).await;
        }));
    }

    pub async fn stop_realtime(&self) {
        if let Some(task) = self.realtime.lock().await.take() {
            task.abort();
        }
    }

    async fn realtime_loop(&self, app: AppHandle) {
        let mut attempt = 0u32;
        loop {
            self.set_realtime(
                &app,
                if attempt == 0 {
                    "connecting"
                } else {
                    "reconnecting"
                },
            )
            .await;
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
                            if message.is_close() {
                                break;
                            }
                        }
                        Ok(Some(Err(error))) => return Err(anyhow!(error)),
                        Ok(None) => break,
                        Err(_) => {
                            let _ = self.sync_once(&app, false).await;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
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
            if store.syncing {
                return Ok(());
            }
            store.syncing = true;
            store.state.status = "正在同步…".into();
        }
        self.emit_state(app).await;
        let result = self.sync_pass(app).await;
        let mut store = self.store.lock().await;
        store.syncing = false;
        match &result {
            Ok((sent, received, deleted)) => {
                let summary = if *sent > 0 || *received > 0 || *deleted > 0 {
                    format!("同步完成：发送 {sent} 条，接收 {received} 条，删除 {deleted} 条")
                } else {
                    "已是最新状态".into()
                };
                store.state.status.clone_from(&summary);
                if manual || *sent > 0 || *received > 0 || *deleted > 0 {
                    store.add_activity("success", summary);
                }
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

    async fn sync_pass(&self, app: &AppHandle) -> Result<(usize, usize, usize)> {
        let sent = match self.scan_outbox(app).await {
            Ok(sent) => sent,
            Err(error) => {
                let mut store = self.store.lock().await;
                store.add_activity("error", format!("Outbox 自动发送失败：{error}"));
                drop(store);
                self.emit_state(app).await;
                0
            }
        };
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
            if page.events.is_empty() {
                break;
            }
            for event in page.events {
                if event.event_type == "transfer.created" {
                    if let Some(payload) = event.payload {
                        let transfer: Transfer = serde_json::from_value(payload)?;
                        if transfer.source_device_id != device.id
                            && self.receive_transfer(app, &endpoint.api, &transfer).await?
                        {
                            received += 1;
                        }
                    }
                } else if event.event_type == "transfer.deleted"
                    && self.delete_transfer(&event.transfer_id).await?
                {
                    deleted += 1;
                }
                cursor = cursor.max(event.sequence);
            }
            endpoint.api.update_cursor(cursor).await?;
            let mut store = self.store.lock().await;
            store.state.sync.cursor = cursor;
            store.save()?;
        }
        Ok((sent, received, deleted))
    }

    async fn scan_outbox(&self, app: &AppHandle) -> Result<usize> {
        let _guard = self.outbox_lock.lock().await;
        let (outbox, sent_files, paired) = {
            let store = self.store.lock().await;
            (
                PathBuf::from(&store.state.settings.outbox_path),
                store.state.sync.sent_files.clone(),
                store.public().paired,
            )
        };
        if !paired {
            return Ok(0);
        }
        tokio::fs::create_dir_all(&outbox).await?;
        let mut directory = tokio::fs::read_dir(&outbox).await?;
        let mut existing = HashSet::new();
        let mut candidates = Vec::new();
        while let Some(entry) = directory.next_entry().await? {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let metadata = entry.metadata().await?;
            if !metadata.is_file() {
                continue;
            }
            let canonical = tokio::fs::canonicalize(&path).await?;
            let key = canonical.to_string_lossy().into_owned();
            existing.insert(key.clone());
            let stable = metadata
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age >= Duration::from_secs(2));
            if !stable {
                continue;
            }
            let signature = lan_peer::metadata_signature(&metadata);
            if sent_files
                .get(&key)
                .is_some_and(|record| record.signature == signature)
            {
                continue;
            }
            if candidates.len() < 64 {
                candidates.push((key, signature));
            }
        }
        {
            let mut store = self.store.lock().await;
            let before = store.state.sync.sent_files.len();
            store
                .state
                .sync
                .sent_files
                .retain(|path, _| existing.contains(path));
            if store.state.sync.sent_files.len() != before {
                store.save()?;
            }
        }
        if candidates.is_empty() {
            return Ok(0);
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        let paths = candidates
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let prepared = outbound::prepare_files(&paths).await?;
        if prepared.is_empty() {
            return Ok(0);
        }
        let mut client_hash = Sha256::new();
        for (path, signature) in &candidates {
            client_hash.update(path.as_bytes());
            client_hash.update([0]);
            client_hash.update(signature.as_bytes());
            client_hash.update([0]);
        }
        let client_id = format!("outbox-{:x}", client_hash.finalize());
        let endpoint = endpoint::resolve(&self.store, &self.client, true).await?;
        let transfer =
            outbound::send_draft_with_id(&endpoint.api, "", &prepared, client_id).await?;
        let signatures = candidates.into_iter().collect::<HashMap<_, _>>();
        {
            let mut store = self.store.lock().await;
            for file in &prepared {
                let path = file.path.to_string_lossy().into_owned();
                let signature = signatures.get(&path).cloned().unwrap_or_else(|| {
                    std::fs::metadata(&file.path)
                        .map(|metadata| lan_peer::metadata_signature(&metadata))
                        .unwrap_or_default()
                });
                store.state.sync.sent_files.insert(
                    path.clone(),
                    SentFile {
                        signature: signature.clone(),
                        transfer_id: transfer.id.clone(),
                    },
                );
                store.state.sync.peer_sources.insert(
                    file.sha256.clone(),
                    PeerSource {
                        path,
                        size: file.size,
                        signature,
                        name: file.name.clone(),
                        mime_type: file.mime_type.clone(),
                        updated_at: chrono::Utc::now().timestamp_millis(),
                    },
                );
            }
            store.state.status = format!("Outbox 已自动发送 {} 个文件", prepared.len());
            store.add_activity(
                "sent",
                format!("Outbox 已自动发送 {} 个文件", prepared.len()),
            );
            store.save()?;
        }
        self.emit_state(app).await;
        let port = self.store.lock().await.state.connection.lan_peer_port;
        if port > 0 {
            self.announce_peer(app, port).await;
        }
        Ok(1)
    }

    async fn receive_transfer(
        &self,
        app: &AppHandle,
        api: &crate::api::OTransApi,
        transfer: &Transfer,
    ) -> Result<bool> {
        let inbox = {
            let store = self.store.lock().await;
            if store.state.sync.transfers.contains_key(&transfer.id) {
                return Ok(false);
            }
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
            let mut direct_sources = HashSet::new();
            let mut direct_failures = Vec::new();
            let mut file_count = 0usize;
            let mut direct_file_count = 0usize;
            for item in &transfer.items {
                if item.kind == "text" {
                    text_index += 1;
                    let name = unique_name(&format!("文本-{text_index}.txt"), &mut used);
                    tokio::fs::write(temporary.join(name), item.text.as_deref().unwrap_or(""))
                        .await?;
                } else if item.kind == "file" {
                    file_count += 1;
                    let name = unique_name(
                        &sanitize_file_name(item.name.as_deref().unwrap_or("未命名文件")),
                        &mut used,
                    );
                    let outcome = self
                        .download_file(api, item, &temporary.join(&name))
                        .await?;
                    if let Some(device_name) = outcome.direct_source {
                        direct_file_count += 1;
                        direct_sources.insert(device_name);
                    }
                    if let Some(reason) = outcome.direct_failure {
                        direct_failures.push(reason);
                    }
                    received_files.push((item.clone(), name));
                }
            }
            tokio::fs::write(
                temporary.join(".otrans-transfer.json"),
                serde_json::to_vec_pretty(transfer)?,
            )
            .await?;
            tokio::fs::rename(&temporary, &folder).await?;
            let mut store = self.store.lock().await;
            for (item, name) in received_files {
                if let Some(digest) = item.sha256 {
                    let path = folder.join(&name);
                    let metadata = tokio::fs::metadata(&path).await?;
                    store.state.sync.peer_sources.insert(
                        digest,
                        PeerSource {
                            path: path.to_string_lossy().into_owned(),
                            size: metadata.len(),
                            signature: file_signature(&metadata),
                            name,
                            mime_type: item
                                .mime_type
                                .unwrap_or_else(|| "application/octet-stream".into()),
                            updated_at: chrono::Utc::now().timestamp_millis(),
                        },
                    );
                }
            }
            store.state.sync.transfers.insert(
                transfer.id.clone(),
                ManagedTransfer {
                    folder: folder.to_string_lossy().into_owned(),
                    route: if file_count == 0 {
                        "text"
                    } else if direct_file_count == file_count {
                        "lan"
                    } else if direct_file_count > 0 {
                        "mixed"
                    } else {
                        "nas"
                    }
                    .into(),
                },
            );
            let title = transfer.title.as_deref().unwrap_or("未命名中转内容");
            let route = if file_count == 0 {
                "text"
            } else if direct_file_count == file_count {
                "lan"
            } else if direct_file_count > 0 {
                "mixed"
            } else {
                "nas"
            };
            let source_names = direct_sources.into_iter().collect::<Vec<_>>().join("、");
            let message = match route {
                "lan" => format!("收到：{title} · 来源 {source_names}"),
                "mixed" => format!(
                    "收到：{title} · {direct_file_count}/{file_count} 个文件来自 {source_names}"
                ),
                "nas" if !direct_failures.is_empty() => {
                    format!("收到：{title} · 直传回退原因：{}", direct_failures[0])
                }
                _ => format!("收到：{title}"),
            };
            store.add_activity_with_route("received", message, Some(route));
            let route_label = match route {
                "lan" => "局域网直传",
                "mixed" => "局域网直传 + NAS",
                "nas" => "经 NAS 下载",
                _ => "纯文本",
            };
            let notification_body = format!("{route_label} · {title}");
            if let Err(error) = app
                .notification()
                .builder()
                .title("OTrans 收到新内容")
                .body(&notification_body)
                .show()
            {
                store.add_activity("error", format!("Windows 通知发送失败：{error}"));
            }
            let _ = app.emit(
                "incoming-transfer",
                serde_json::json!({
                    "title": title,
                    "route": route_label
                }),
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&temporary).await;
        }
        result?;
        let port = self.store.lock().await.state.connection.lan_peer_port;
        if port > 0 {
            self.announce_peer(app, port).await;
        }
        Ok(true)
    }

    async fn download_file(
        &self,
        api: &crate::api::OTransApi,
        item: &TransferItem,
        destination: &Path,
    ) -> Result<DownloadOutcome> {
        let digest = item
            .sha256
            .as_deref()
            .ok_or_else(|| anyhow!("文件记录缺少 SHA-256"))?;
        let name = item.name.as_deref().unwrap_or("file");
        let lan_enabled = self.store.lock().await.state.settings.lan_peer_enabled;
        let mut failure = None;
        if lan_enabled {
            match api.peer_sources(digest).await {
                Ok(peers) if peers.sources.is_empty() => {
                    failure = Some("没有设备登记该文件".into());
                }
                Ok(peers) => {
                    for peer in peers.sources {
                        let _ = (&peer.device_id, &peer.expires_at);
                        for address in &peer.addresses {
                            let Ok(mut url) = url::Url::parse(address) else {
                                failure = Some("来源设备登记了无效地址".into());
                                continue;
                            };
                            url.set_path(&format!("/v1/blobs/{digest}"));
                            url.query_pairs_mut()
                                .clear()
                                .append_pair("ticket", &peer.ticket);
                            match self.peer_client.get(url).send().await {
                                Ok(response) if response.status().is_success() => {
                                    match self
                                        .save_checked_response(response, item, destination)
                                        .await
                                    {
                                        Ok(()) => {
                                            return Ok(DownloadOutcome {
                                                direct_source: Some(peer.device_name),
                                                direct_failure: None,
                                            });
                                        }
                                        Err(error) => {
                                            failure = Some(format!("直传内容校验失败：{error}"));
                                        }
                                    }
                                }
                                Ok(response) => {
                                    failure =
                                        Some(format!("来源设备返回 HTTP {}", response.status()));
                                }
                                Err(error) => {
                                    failure = Some(format!("无法连接来源设备：{error}"));
                                }
                            }
                            let _ = tokio::fs::remove_file(part_path(destination)).await;
                        }
                    }
                }
                Err(error) => failure = Some(format!("查询直传来源失败：{error}")),
            }
        }
        let response = api.get_blob(digest, name).await?;
        self.save_checked_response(response, item, destination)
            .await?;
        Ok(DownloadOutcome {
            direct_source: None,
            direct_failure: failure,
        })
    }

    async fn save_checked_response(
        &self,
        response: reqwest::Response,
        item: &TransferItem,
        destination: &Path,
    ) -> Result<()> {
        let digest = item
            .sha256
            .as_deref()
            .ok_or_else(|| anyhow!("文件记录缺少 SHA-256"))?;
        let name = item.name.as_deref().unwrap_or("file");
        let temporary = part_path(destination);
        let _ = tokio::fs::remove_file(&temporary).await;
        let mut output = tokio::fs::File::create(&temporary).await?;
        let mut stream = response.bytes_stream();
        let mut hash = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size = size.saturating_add(chunk.len() as u64);
            if item.size.is_some_and(|expected| size > expected) {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(anyhow!("{name} 文件大小校验失败"));
            }
            hash.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        drop(output);
        if item.size.is_some_and(|expected| expected != size) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(anyhow!("{name} 文件大小校验失败"));
        }
        let actual = format!("{:x}", hash.finalize());
        if actual != digest.to_lowercase() {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(anyhow!("{name} 完整性校验失败"));
        }
        tokio::fs::rename(&temporary, destination).await?;
        Ok(())
    }

    async fn delete_transfer(&self, id: &str) -> Result<bool> {
        let (inbox, folder) = {
            let store = self.store.lock().await;
            let Some(record) = store.state.sync.transfers.get(id) else {
                return Ok(false);
            };
            (
                PathBuf::from(&store.state.settings.inbox_path),
                PathBuf::from(&record.folder),
            )
        };
        if !is_inside(&inbox, &folder) {
            return Err(anyhow!("拒绝删除 Inbox 之外的路径"));
        }
        if let Err(error) = tokio::fs::remove_dir_all(&folder).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
        let mut store = self.store.lock().await;
        store.state.sync.transfers.remove(id);
        store.add_activity(
            "deleted",
            format!(
                "已同步删除：{}",
                folder
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("中转内容")
            ),
        );
        Ok(true)
    }
}

fn sanitize_file_name(value: &str) -> String {
    let mut name: String = value
        .trim()
        .chars()
        .map(|c| {
            if "\\/:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    while name.ends_with(['.', ' ']) {
        name.pop();
    }
    if name.is_empty() || name == "." || name == ".." {
        name = "未命名文件".into();
    }
    let upper = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        name.insert(0, '_');
    }
    name.chars().take(180).collect()
}

fn unique_name(candidate: &str, used: &mut HashSet<String>) -> String {
    let path = Path::new(candidate);
    let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("文件");
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| format!(".{v}"))
        .unwrap_or_default();
    let mut result = candidate.to_string();
    let mut suffix = 2;
    while !used.insert(result.to_lowercase()) {
        result = format!("{stem} ({suffix}){extension}");
        suffix += 1;
    }
    result
}

fn transfer_folder_name(transfer: &Transfer) -> String {
    let timestamp = transfer
        .committed_at
        .as_deref()
        .unwrap_or(&transfer.created_at);
    let stamp = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y%m%d-%H%M%S")
                .to_string()
        })
        .unwrap_or_else(|_| "unknown-time".into());
    let files = transfer
        .items
        .iter()
        .filter(|item| item.kind == "file")
        .collect::<Vec<_>>();
    let label = if let Some(first) = files.first() {
        let name = sanitize_file_name(first.name.as_deref().unwrap_or("文件"));
        let stem = Path::new(&name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("文件");
        let mut label = screenshot_label(stem).unwrap_or_else(|| {
            let prefix = if first
                .mime_type
                .as_deref()
                .unwrap_or("")
                .starts_with("image/")
            {
                "图片 "
            } else {
                ""
            };
            format!("{prefix}{stem}")
        });
        if transfer.items.len() > 1 {
            label.push_str(&format!(" 等{}项", transfer.items.len()));
        }
        label
    } else if transfer.items.iter().any(|item| item.kind == "text") {
        "文本".into()
    } else {
        let title = transfer.title.as_deref().unwrap_or("中转内容");
        if title.starts_with("来自") {
            title
                .split_once(['：', ':'])
                .map(|(_, value)| value)
                .unwrap_or("中转内容")
                .into()
        } else {
            title.into()
        }
    };
    let concise: String = sanitize_file_name(&label).chars().take(42).collect();
    format!("{stamp} {concise}")
}

fn screenshot_label(stem: &str) -> Option<String> {
    let lower = stem.to_ascii_lowercase();
    let remainder = ["screenshot", "screen_capture", "screencapture", "截图"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .map(|value| value.trim_start_matches(['_', '-', ' ']))?;
    let bytes = remainder.as_bytes();
    if bytes.len() < 15
        || !bytes[..8].iter().all(u8::is_ascii_digit)
        || !matches!(bytes[8], b'_' | b'-')
        || !bytes[9..15].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(format!("截图 {}-{}", &remainder[..8], &remainder[9..15]))
}

async fn unique_folder(inbox: &Path, base_name: &str) -> Result<PathBuf> {
    for suffix in 1..1000 {
        let name = if suffix == 1 {
            base_name.to_string()
        } else {
            format!("{base_name} ({suffix})")
        };
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

fn part_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    destination.with_file_name(format!(".{name}.part"))
}

fn file_signature(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{}:{modified}", metadata.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concise_screenshot_folder_drops_device_prefix_and_id() {
        let transfer = Transfer {
            id: "af547b85-rest".into(),
            source_device_id: "phone".into(),
            title: Some("来自 HarmonyOS：screenshot_20261017_124314_com.example".into()),
            status: "committed".into(),
            created_at: "2026-10-17T04:43:42Z".into(),
            committed_at: Some("2026-10-17T04:43:42Z".into()),
            items: vec![TransferItem {
                id: None,
                kind: "file".into(),
                text: None,
                name: Some("screenshot_20261017_124314_com.example.png".into()),
                mime_type: Some("image/png".into()),
                size: Some(1),
                sha256: Some("0".repeat(64)),
            }],
        };
        assert_eq!(
            transfer_folder_name(&transfer),
            "20261017-124342 截图 20261017-124314"
        );
    }
}
