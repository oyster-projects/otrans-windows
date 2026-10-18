#[cfg(windows)]
mod platform {
    use std::{
        ptr,
        sync::{OnceLock, mpsc},
        thread,
    };

    use tauri::AppHandle;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
            MSG, RegisterClassW, TranslateMessage, WM_POWERBROADCAST, WNDCLASSW,
        },
    };

    use crate::sync::AppRuntime;

    const PBT_APMSUSPEND: usize = 0x0004;
    const PBT_APMRESUMESUSPEND: usize = 0x0007;
    const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

    #[derive(Clone, Copy)]
    enum PowerEvent {
        Suspend,
        Resume,
    }

    static POWER_EVENTS: OnceLock<mpsc::Sender<PowerEvent>> = OnceLock::new();

    pub fn start(app: AppHandle, runtime: AppRuntime) {
        let (sender, receiver) = mpsc::channel();
        if POWER_EVENTS.set(sender).is_err() {
            return;
        }
        thread::Builder::new()
            .name("otrans-power-events".into())
            .spawn(message_window)
            .expect("power event thread");
        thread::Builder::new()
            .name("otrans-power-actions".into())
            .spawn(move || {
                let mut last_resume = None;
                while let Ok(event) = receiver.recv() {
                    if matches!(event, PowerEvent::Resume) {
                        let now = std::time::Instant::now();
                        if last_resume.is_some_and(|last| {
                            now.duration_since(last) < std::time::Duration::from_secs(3)
                        }) {
                            continue;
                        }
                        last_resume = Some(now);
                    }
                    let app = app.clone();
                    let runtime = runtime.clone();
                    tauri::async_runtime::spawn(async move {
                        match event {
                            PowerEvent::Suspend => runtime.stop_services().await,
                            PowerEvent::Resume => {
                                {
                                    let mut store = runtime.store.lock().await;
                                    store.add_activity("info", "系统已恢复，正在重连服务");
                                }
                                runtime.restart_services(app.clone()).await;
                                let _ = runtime.sync_once(&app, false).await;
                            }
                        }
                    });
                }
            })
            .expect("power action thread");
    }

    fn message_window() {
        let class_name = "OTransPowerMonitor\0".encode_utf16().collect::<Vec<_>>();
        unsafe {
            let module = GetModuleHandleW(ptr::null());
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: module,
                lpszClassName: class_name.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&class) == 0 {
                return;
            }
            let window = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                module,
                ptr::null(),
            );
            if window.is_null() {
                return;
            }
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_POWERBROADCAST {
            let event = match wparam {
                PBT_APMSUSPEND => Some(PowerEvent::Suspend),
                PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resume),
                _ => None,
            };
            if let Some(event) = event
                && let Some(sender) = POWER_EVENTS.get()
            {
                let _ = sender.send(event);
            }
            return 1;
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }
}

#[cfg(windows)]
pub use platform::start;

#[cfg(not(windows))]
pub fn start(_app: tauri::AppHandle, _runtime: crate::sync::AppRuntime) {}
