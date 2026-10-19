use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use keyring::Entry;
use uuid::Uuid;

use crate::models::{Activity, ConnectionState, PersistedState, PublicState, Settings, SyncState};

const TOKEN_SERVICE: &str = "com.otrans.desktop.rust";
const TOKEN_ACCOUNT: &str = "device-token";

pub struct StateStore {
    path: PathBuf,
    pub state: PersistedState,
    pub syncing: bool,
}

impl StateStore {
    pub fn load(path: PathBuf) -> Self {
        let defaults = Self::defaults();
        let backup = path.with_extension("json.bak");
        let state = [path.as_path(), backup.as_path()]
            .iter()
            .find_map(|candidate| {
                fs::read_to_string(candidate)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<PersistedState>(&raw).ok())
            })
            .unwrap_or(defaults);
        Self {
            path,
            state,
            syncing: false,
        }
    }

    pub fn defaults() -> PersistedState {
        let documents = dirs::document_dir().unwrap_or_else(|| PathBuf::from("."));
        PersistedState {
            status: "尚未同步".into(),
            settings: Settings {
                lan_server_url: "http://192.168.1.10:18990".into(),
                wan_server_url: String::new(),
                server_id: String::new(),
                device_name: format!(
                    "Windows-{}",
                    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
                ),
                inbox_path: documents
                    .join("OTrans")
                    .join("Inbox")
                    .to_string_lossy()
                    .into_owned(),
                outbox_path: documents
                    .join("OTrans")
                    .join("Outbox")
                    .to_string_lossy()
                    .into_owned(),
                start_at_login: false,
                lan_peer_enabled: false,
            },
            sync: SyncState::default(),
            connection: ConnectionState {
                active_route: "offline".into(),
                realtime_status: "disconnected".into(),
                lan_peer_status: "disabled".into(),
                lan_peer_detail: "局域网文件服务未启动".into(),
                ..ConnectionState::default()
            },
            activity: Vec::new(),
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let backup = self.path.with_extension("json.bak");
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        let mut file = fs::File::create(&temporary)?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if self.path.exists() {
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(&self.path, &backup)?;
        }
        if let Err(error) = fs::rename(&temporary, &self.path) {
            if backup.exists() && !self.path.exists() {
                let _ = fs::rename(&backup, &self.path);
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub fn public(&self) -> PublicState {
        PublicState {
            settings: self.state.settings.clone(),
            paired: self.token().is_ok_and(|token| !token.is_empty()),
            device_id: self.state.sync.device_id.clone(),
            status: self.state.status.clone(),
            syncing: self.syncing,
            connection: self.state.connection.clone(),
            lan_peer_source_count: self.state.sync.peer_sources.len(),
            activity: self.state.activity.iter().take(40).cloned().collect(),
        }
    }

    pub fn token(&self) -> Result<String> {
        match Entry::new(TOKEN_SERVICE, TOKEN_ACCOUNT)?.get_password() {
            Ok(token) => Ok(token),
            Err(keyring::Error::NoEntry) => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn set_token(&self, token: &str) -> Result<()> {
        Entry::new(TOKEN_SERVICE, TOKEN_ACCOUNT)?.set_password(token)?;
        Ok(())
    }

    pub fn clear_token(&mut self) -> Result<()> {
        let entry = Entry::new(TOKEN_SERVICE, TOKEN_ACCOUNT)?;
        if let Err(error) = entry.delete_credential()
            && !matches!(error, keyring::Error::NoEntry)
        {
            return Err(error.into());
        }
        self.state.sync.cursor = 0;
        self.state.sync.device_id.clear();
        self.state.sync.peer_sources.clear();
        self.save()
    }

    pub fn add_activity(&mut self, kind: &str, message: impl Into<String>) {
        self.add_activity_with_route(kind, message, None);
    }

    pub fn add_activity_with_route(
        &mut self,
        kind: &str,
        message: impl Into<String>,
        route: Option<&str>,
    ) {
        self.state.activity.insert(
            0,
            Activity {
                id: Uuid::new_v4().to_string(),
                kind: kind.into(),
                message: message.into(),
                at: Utc::now().to_rfc3339(),
                route: route.map(str::to_owned),
            },
        );
        self.state.activity.truncate(100);
        let _ = self.save();
    }

    pub fn ensure_directories(&self) -> Result<()> {
        fs::create_dir_all(&self.state.settings.inbox_path)
            .with_context(|| format!("无法创建接收目录 {}", self.state.settings.inbox_path))?;
        fs::create_dir_all(&self.state.settings.outbox_path)
            .with_context(|| format!("无法创建发送目录 {}", self.state.settings.outbox_path))?;
        Ok(())
    }
}
