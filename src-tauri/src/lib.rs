mod api;
mod commands;
mod endpoint;
mod lan_peer;
mod models;
mod outbound;
mod state;
mod sync;
mod windows_power;

use tauri::{
    Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_autostart::MacosLauncher;

use sync::AppRuntime;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let store = state::StateStore::load(data_dir.join("state.json"));
            store.ensure_directories()?;
            let runtime = AppRuntime::new(store);
            app.manage(runtime.clone());
            windows_power::start(app.handle().clone(), runtime.clone());

            let open = MenuItem::with_id(app, "open", "打开 OTrans", true, None::<&str>)?;
            let inbox = MenuItem::with_id(app, "inbox", "打开接收文件夹", true, None::<&str>)?;
            let outbox = MenuItem::with_id(app, "outbox", "打开发送文件夹", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &inbox, &outbox, &quit])?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .expect("application icon"),
                )
                .tooltip("OTrans 中转站")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "inbox" => {
                        if let Some(runtime) = app.try_state::<AppRuntime>() {
                            let runtime = runtime.inner().clone();
                            tauri::async_runtime::spawn(async move {
                                let path =
                                    runtime.store.lock().await.state.settings.inbox_path.clone();
                                let _ =
                                    std::process::Command::new("explorer.exe").arg(path).spawn();
                            });
                        }
                    }
                    "outbox" => {
                        if let Some(runtime) = app.try_state::<AppRuntime>() {
                            let runtime = runtime.inner().clone();
                            tauri::async_runtime::spawn(async move {
                                let path = runtime
                                    .store
                                    .lock()
                                    .await
                                    .state
                                    .settings
                                    .outbox_path
                                    .clone();
                                let _ =
                                    std::process::Command::new("explorer.exe").arg(path).spawn();
                            });
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(event, tauri::tray::TrayIconEvent::DoubleClick { .. }) {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            if !std::env::args().any(|arg| arg == "--hidden") {
                show_main(app.handle());
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                runtime.restart_services(handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::save_settings,
            commands::pair,
            commands::disconnect,
            commands::send_draft,
            commands::sync_now,
            commands::choose_files,
            commands::choose_directory,
            commands::open_path,
            commands::read_clipboard,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OTrans");
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
