#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod settings;

use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_store::StoreExt;

pub struct AppState {
    pub recording: Arc<Mutex<bool>>,
    pub stop_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
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

    // Get API key
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let api_key = store
        .get("api_key")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .ok_or_else(|| "API key not configured. Open Settings to add it.".to_string())?;

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut stop = state.stop_tx.lock().unwrap();
        *stop = Some(stop_tx);
        let mut recording = state.recording.lock().unwrap();
        *recording = true;
    }

    let app_handle = app.clone();
    tokio::spawn(async move {
        if let Err(e) = audio::run_capture(app_handle.clone(), api_key, stop_rx).await {
            let _ = app_handle.emit("transcript-error", e.to_string());
        }
        let state = app_handle.state::<AppState>();
        let mut recording = state.recording.lock().unwrap();
        *recording = false;
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
async fn get_api_key(app: AppHandle) -> Result<Option<String>, String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    Ok(store.get("api_key").and_then(|v| v.as_str().map(|s| s.to_string())))
}

#[tauri::command]
async fn set_api_key(app: AppHandle, key: String) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    store.set("api_key", serde_json::Value::String(key));
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn set_always_on_top(app: AppHandle, on_top: bool) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.set_always_on_top(on_top).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn list_audio_devices() -> Result<Vec<String>, String> {
    audio::list_devices().map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(AppState {
            recording: Arc::new(Mutex::new(false)),
            stop_tx: Arc::new(Mutex::new(None)),
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            get_api_key,
            set_api_key,
            set_always_on_top,
            list_audio_devices,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
