#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod settings;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "settings.json";
const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+S";

pub struct AppState {
    pub recording: Arc<Mutex<bool>>,
    pub stop_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    pub mic_preview_stop: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    pub current_shortcut: Arc<Mutex<Option<Shortcut>>>,
}

fn get_str(app: &AppHandle, key: &str) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store.get(key).and_then(|v| v.as_str().map(|s| s.to_string()))
}

#[tauri::command]
async fn start_recording(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let recording = state.recording.lock().unwrap();
        if *recording {
            return Err("Already recording".into());
        }
    }

    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let api_key = store
        .get("api_key")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "API key not configured. Open Settings to add it.".to_string())?;
    let device_name = store
        .get("device_name")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty() && s != "Default");
    let language = store
        .get("language")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty() && s != "auto");

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut stop = state.stop_tx.lock().unwrap();
        *stop = Some(stop_tx);
        let mut recording = state.recording.lock().unwrap();
        *recording = true;
    }

    let app_handle = app.clone();
    tokio::spawn(async move {
        if let Err(e) =
            audio::run_capture(app_handle.clone(), api_key, device_name, language, stop_rx).await
        {
            let _ = app_handle.emit("transcript-error", e.to_string());
        }
        let state = app_handle.state::<AppState>();
        let mut recording = state.recording.lock().unwrap();
        *recording = false;
        let _ = app_handle.emit("recording-state", false);
    });

    app.emit("recording-state", true).ok();
    Ok(())
}

#[tauri::command]
async fn stop_recording(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut stop = state.stop_tx.lock().unwrap();
    if let Some(tx) = stop.take() {
        let _ = tx.send(());
    }
    app.emit("recording-state", false).ok();
    Ok(())
}

#[tauri::command]
async fn toggle_recording(app: AppHandle) -> Result<(), String> {
    let is_recording = {
        let state = app.state::<AppState>();
        let r = state.recording.lock().unwrap();
        *r
    };
    if is_recording {
        stop_recording(app).await
    } else {
        start_recording(app).await
    }
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<serde_json::Value, String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let mut obj = serde_json::Map::new();
    for k in [
        "api_key",
        "device_name",
        "language",
        "opacity",
        "font_size",
        "max_words",
        "auto_start",
        "shortcut",
        "show_advanced",
    ] {
        if let Some(v) = store.get(k) {
            obj.insert(k.to_string(), v);
        }
    }
    Ok(serde_json::Value::Object(obj))
}

#[tauri::command]
async fn set_settings(app: AppHandle, values: serde_json::Value) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    if let serde_json::Value::Object(map) = values {
        for (k, v) in map {
            store.set(k, v);
        }
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn reset_settings(app: AppHandle) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    for k in [
        "api_key",
        "device_name",
        "language",
        "opacity",
        "font_size",
        "max_words",
        "auto_start",
        "shortcut",
        "show_advanced",
    ] {
        store.delete(k);
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_api_key(app: AppHandle) -> Result<Option<String>, String> {
    Ok(get_str(&app, "api_key"))
}

#[tauri::command]
async fn set_api_key(app: AppHandle, key: String) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set("api_key", serde_json::Value::String(key));
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn test_api_key(key: String) -> Result<bool, String> {
    if key.trim().is_empty() {
        return Err("Empty key".to_string());
    }
    match audio::test_api_key(key).await {
        Ok(()) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn set_always_on_top(app: AppHandle, on_top: bool) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.set_always_on_top(on_top).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn list_audio_devices() -> Result<Vec<audio::AudioDevice>, String> {
    audio::list_devices().map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_mic_preview(app: AppHandle, device_name: Option<String>) -> Result<(), String> {
    // Stop existing preview if any
    let state = app.state::<AppState>();
    {
        let mut cur = state.mic_preview_stop.lock().unwrap();
        if let Some(flag) = cur.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let mut cur = state.mic_preview_stop.lock().unwrap();
        *cur = Some(stop_flag.clone());
    }
    audio::run_mic_preview(app.clone(), device_name, stop_flag).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn stop_mic_preview(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut cur = state.mic_preview_stop.lock().unwrap();
    if let Some(flag) = cur.take() {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Parse a shortcut string like "CommandOrControl+Shift+S" into a Shortcut.
fn parse_shortcut(s: &str) -> Option<Shortcut> {
    let mut mods = Modifiers::empty();
    let mut key: Option<Code> = None;
    for part in s.split('+') {
        let p = part.trim();
        match p.to_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => {
                mods |= Modifiers::CONTROL
            }
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "super" | "meta" | "cmd" | "command" | "win" => mods |= Modifiers::SUPER,
            _ => {
                key = code_from_name(p);
            }
        }
    }
    key.map(|k| Shortcut::new(Some(mods), k))
}

fn code_from_name(s: &str) -> Option<Code> {
    let u = s.to_uppercase();
    Some(match u.as_str() {
        "A" => Code::KeyA,
        "B" => Code::KeyB,
        "C" => Code::KeyC,
        "D" => Code::KeyD,
        "E" => Code::KeyE,
        "F" => Code::KeyF,
        "G" => Code::KeyG,
        "H" => Code::KeyH,
        "I" => Code::KeyI,
        "J" => Code::KeyJ,
        "K" => Code::KeyK,
        "L" => Code::KeyL,
        "M" => Code::KeyM,
        "N" => Code::KeyN,
        "O" => Code::KeyO,
        "P" => Code::KeyP,
        "Q" => Code::KeyQ,
        "R" => Code::KeyR,
        "S" => Code::KeyS,
        "T" => Code::KeyT,
        "U" => Code::KeyU,
        "V" => Code::KeyV,
        "W" => Code::KeyW,
        "X" => Code::KeyX,
        "Y" => Code::KeyY,
        "Z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        "SPACE" | "SPC" => Code::Space,
        "ENTER" | "RETURN" => Code::Enter,
        _ => return None,
    })
}

fn register_app_shortcut(app: &AppHandle, accel: &str) -> Result<(), String> {
    let shortcut = parse_shortcut(accel).ok_or_else(|| format!("Invalid shortcut: {}", accel))?;
    let state = app.state::<AppState>();
    // Unregister old
    {
        let mut cur = state.current_shortcut.lock().unwrap();
        if let Some(old) = cur.take() {
            let _ = app.global_shortcut().unregister(old);
        }
    }
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| e.to_string())?;
    let mut cur = state.current_shortcut.lock().unwrap();
    *cur = Some(shortcut);
    Ok(())
}

#[tauri::command]
async fn register_shortcut(app: AppHandle, accel: String) -> Result<(), String> {
    register_app_shortcut(&app, &accel)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let _ = app.emit("shortcut-toggle-recording", ());
                    }
                })
                .build(),
        )
        .manage(AppState {
            recording: Arc::new(Mutex::new(false)),
            stop_tx: Arc::new(Mutex::new(None)),
            mic_preview_stop: Arc::new(Mutex::new(None)),
            current_shortcut: Arc::new(Mutex::new(None)),
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            toggle_recording,
            get_api_key,
            set_api_key,
            test_api_key,
            set_always_on_top,
            list_audio_devices,
            start_mic_preview,
            stop_mic_preview,
            get_settings,
            set_settings,
            reset_settings,
            register_shortcut,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // Register saved shortcut (or default)
            let accel = get_str(&handle, "shortcut").unwrap_or_else(|| DEFAULT_SHORTCUT.to_string());
            if let Err(e) = register_app_shortcut(&handle, &accel) {
                eprintln!("[shortcut] register failed: {}", e);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
