use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, anyhow};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

use crate::{
    endpoint,
    models::{ClipboardContent, PairInput, PublicState, SaveSettingsInput, SendDraftInput},
    outbound,
    sync::AppRuntime,
};

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tauri::command]
pub async fn get_state(runtime: State<'_, AppRuntime>) -> Result<PublicState, String> {
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub fn get_app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
    input: SaveSettingsInput,
) -> Result<PublicState, String> {
    if input.lan_server_url.trim().is_empty() && input.wan_server_url.trim().is_empty() {
        return Err("请至少填写一个飞牛服务地址".into());
    }
    for url in [&input.lan_server_url, &input.wan_server_url] {
        if !url.trim().is_empty() {
            crate::api::OTransApi::new(url, "", runtime.client.clone()).map_err(message)?;
        }
    }
    {
        let mut store = runtime.store.lock().await;
        store.state.settings.lan_server_url =
            input.lan_server_url.trim().trim_end_matches('/').into();
        store.state.settings.wan_server_url =
            input.wan_server_url.trim().trim_end_matches('/').into();
        store.state.settings.device_name = if input.device_name.trim().is_empty() {
            "Windows PC".into()
        } else {
            input.device_name.trim().into()
        };
        store.state.settings.inbox_path = absolute_path(&input.inbox_path)
            .map_err(message)?
            .to_string_lossy()
            .into_owned();
        store.state.settings.outbox_path = absolute_path(&input.outbox_path)
            .map_err(message)?
            .to_string_lossy()
            .into_owned();
        store.state.settings.start_at_login = input.start_at_login;
        store.state.settings.lan_peer_enabled = input.lan_peer_enabled;
        store.ensure_directories().map_err(message)?;
        store.save().map_err(message)?;
    }
    if input.start_at_login {
        app.autolaunch().enable().map_err(message)?;
    } else {
        let _ = app.autolaunch().disable();
    }
    runtime.restart_services(app.clone()).await;
    runtime.emit_state(&app).await;
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub async fn pair(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
    input: PairInput,
) -> Result<PublicState, String> {
    if input.device_name.trim().is_empty() || input.pair_code.trim().is_empty() {
        return Err("请填写设备名称和配对码".into());
    }
    let previous = {
        let mut store = runtime.store.lock().await;
        if store.public().paired {
            return Err("请先断开当前设备，再使用新的配对码连接".into());
        }
        let previous = store.state.settings.clone();
        store.state.settings.lan_server_url =
            input.lan_server_url.trim().trim_end_matches('/').into();
        store.state.settings.wan_server_url =
            input.wan_server_url.trim().trim_end_matches('/').into();
        store.state.settings.server_id.clear();
        previous
    };
    let result = async {
        let endpoint = endpoint::resolve(&runtime.store, &runtime.client, false).await?;
        let paired = endpoint
            .api
            .pair(input.pair_code.trim(), input.device_name.trim())
            .await?;
        let mut store = runtime.store.lock().await;
        store.state.settings.device_name = input.device_name.trim().into();
        store.state.settings.server_id = endpoint.server_id;
        store.state.sync.device_id.clone_from(&paired.device.id);
        store.state.sync.cursor = paired.device.cursor;
        store.set_token(&paired.token)?;
        store.add_activity("success", format!("配对成功：{}", paired.device.name));
        store.save()?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        let mut store = runtime.store.lock().await;
        store.state.settings = previous;
        let _ = store.save();
        return Err(message(error));
    }
    runtime.restart_services(app.clone()).await;
    runtime.emit_state(&app).await;
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub async fn disconnect(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
) -> Result<PublicState, String> {
    runtime.stop_services().await;
    {
        let mut store = runtime.store.lock().await;
        store.clear_token().map_err(message)?;
        store.state.settings.server_id.clear();
        store.state.connection.active_server_url.clear();
        store.state.connection.active_route = "offline".into();
        store.state.connection.realtime_status = "disconnected".into();
        store.state.connection.lan_peer_status = "disabled".into();
        store.state.connection.lan_peer_port = 0;
        store.state.connection.lan_peer_detail = "局域网直传未启用".into();
        store.add_activity("info", "已断开当前设备配对");
        store.save().map_err(message)?;
    }
    runtime.emit_state(&app).await;
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub async fn send_draft(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
    input: SendDraftInput,
) -> Result<PublicState, String> {
    let prepared = outbound::prepare_files(&input.paths)
        .await
        .map_err(message)?;
    let endpoint = endpoint::resolve(&runtime.store, &runtime.client, true)
        .await
        .map_err(message)?;
    let transfer = outbound::send_draft(&endpoint.api, &input.text, &prepared)
        .await
        .map_err(message)?;
    {
        let mut store = runtime.store.lock().await;
        for file in prepared {
            store.state.sync.peer_sources.insert(
                file.sha256.clone(),
                crate::models::PeerSource {
                    path: file.path.to_string_lossy().into_owned(),
                    size: file.size,
                    signature: file_signature(&file.path).unwrap_or_default(),
                    name: file.name,
                    mime_type: file.mime_type,
                    updated_at: chrono::Utc::now().timestamp_millis(),
                },
            );
        }
        store.add_activity(
            "sent",
            format!(
                "已发送：{}",
                transfer.title.as_deref().unwrap_or("中转内容")
            ),
        );
        store.state.status = "内容已经进入中转站".into();
        store.save().map_err(message)?;
    }
    runtime.emit_state(&app).await;
    let port = runtime.store.lock().await.state.connection.lan_peer_port;
    if port > 0 {
        runtime.announce_peer(&app, port).await;
    }
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
) -> Result<PublicState, String> {
    runtime.sync_once(&app, true).await.map_err(message)?;
    Ok(runtime.public_state().await)
}

#[tauri::command]
pub fn test_notification(app: AppHandle) -> Result<String, String> {
    app.notification()
        .builder()
        .title("OTrans 通知测试")
        .body("系统通知通道工作正常")
        .show()
        .map_err(message)?;
    Ok("已请求 Windows 显示通知；便携版不受 Windows 通知支持，请使用安装版验证。".into())
}

#[tauri::command]
pub async fn choose_files() -> Result<Vec<String>, String> {
    let files = rfd::AsyncFileDialog::new()
        .set_title("选择要发送到 OTrans 的文件")
        .pick_files()
        .await;
    Ok(files
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.path().to_string_lossy().into_owned())
        .collect())
}

#[tauri::command]
pub async fn choose_directory(title: String) -> Result<String, String> {
    Ok(rfd::AsyncFileDialog::new()
        .set_title(&title)
        .pick_folder()
        .await
        .map(|folder| folder.path().to_string_lossy().into_owned())
        .unwrap_or_default())
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("路径为空".into());
    }
    Command::new("explorer.exe")
        .arg(&path)
        .spawn()
        .map_err(message)?;
    Ok(())
}

#[tauri::command]
pub async fn read_clipboard(app: AppHandle) -> Result<ClipboardContent, String> {
    #[cfg(windows)]
    {
        use clipboard_win::{
            Clipboard, Getter,
            formats::{FileList, Format},
        };
        if FileList.is_format_avail() {
            let _clipboard = Clipboard::new_attempts(10).map_err(message)?;
            let mut files: Vec<PathBuf> = Vec::new();
            FileList.read_clipboard(&mut files).map_err(message)?;
            let paths = files
                .into_iter()
                .filter(|path| path.is_file())
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if !paths.is_empty() {
                return Ok(ClipboardContent {
                    kind: "files".into(),
                    text: String::new(),
                    paths,
                });
            }
        }
    }
    let mut clipboard = arboard::Clipboard::new().map_err(message)?;
    if let Ok(content) = clipboard.get_image() {
        let directory = app
            .path()
            .app_cache_dir()
            .map_err(message)?
            .join("Clipboard");
        std::fs::create_dir_all(&directory).map_err(message)?;
        let path = directory.join(format!(
            "clipboard-image-{}.png",
            chrono::Utc::now().timestamp_millis()
        ));
        let pixels = content.bytes.into_owned();
        let image = image::RgbaImage::from_raw(content.width as u32, content.height as u32, pixels)
            .ok_or_else(|| "无法转换剪贴板图片".to_string())?;
        image.save(&path).map_err(message)?;
        return Ok(ClipboardContent {
            kind: "image".into(),
            text: String::new(),
            paths: vec![path.to_string_lossy().into_owned()],
        });
    }
    let text = clipboard.get_text().unwrap_or_default();
    Ok(ClipboardContent {
        kind: if text.is_empty() { "empty" } else { "text" }.into(),
        text,
        paths: Vec::new(),
    })
}

fn absolute_path(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.trim());
    if path.as_os_str().is_empty() {
        return Err(anyhow!("路径不能为空"));
    }
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn file_signature(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    Ok(format!("{}:{modified}", metadata.len()))
}
