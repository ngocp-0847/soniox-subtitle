use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const SONIOX_WS_URL: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
const TARGET_SAMPLE_RATE: u32 = 16000;

#[derive(Debug, Deserialize)]
struct SonioxMessage {
    tokens: Option<Vec<SonioxToken>>,
    error: Option<String>,
    error_code: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SonioxToken {
    text: String,
    is_final: Option<bool>,
    translation_status: Option<String>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct TranscriptEvent {
    pub source_stable: String,
    pub source_live: String,
    pub target_stable: String,
    pub target_live: String,
    pub has_target: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    pub is_advanced: bool,
}

fn classify_advanced(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("hw:")
        || n.starts_with("plughw:")
        || n.starts_with("dsnoop:")
        || n.starts_with("dmix:")
        || n.starts_with("front:")
        || n.starts_with("surround")
        || n.starts_with("iec958:")
        || n.starts_with("sysdefault:")
}

pub fn list_devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let mut out: Vec<AudioDevice> = Vec::new();
    out.push(AudioDevice {
        name: "Default".to_string(),
        is_default: true,
        is_advanced: false,
    });

    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                let is_def = name == default_name;
                out.push(AudioDevice {
                    name,
                    is_default: is_def,
                    is_advanced: false,
                });
            }
        }
    }
    // mark advanced after collection
    for d in out.iter_mut() {
        if d.name != "Default" {
            d.is_advanced = classify_advanced(&d.name);
        }
    }
    Ok(out)
}

fn pick_device(name_opt: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match name_opt {
        None | Some("") | Some("Default") => host
            .default_input_device()
            .ok_or_else(|| anyhow!("No default input device")),
        Some(target) => {
            for d in host.input_devices()? {
                if let Ok(n) = d.name() {
                    if n == target {
                        return Ok(d);
                    }
                }
            }
            Err(anyhow!("Device not found: {}", target))
        }
    }
}

fn resample(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == TARGET_SAMPLE_RATE {
        return samples.to_vec();
    }
    let ratio = TARGET_SAMPLE_RATE as f64 / src_rate as f64;
    let new_len = ((samples.len() as f64) * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let pos = i as f64 / ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = samples.get(idx).copied().unwrap_or(0.0);
            let b = samples.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

fn to_pcm_bytes(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect()
}

/// Spawn a CPAL capture thread.
/// `on_mono` receives mono f32 samples at the device's sample rate.
fn spawn_capture(
    device: cpal::Device,
    stop_flag: Arc<AtomicBool>,
    mut on_mono: Box<dyn FnMut(&[f32], u32) + Send + 'static>,
) -> Result<()> {
    let config = device
        .default_input_config()
        .context("Failed to get device config")?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let fmt = config.sample_format();

    std::thread::spawn(move || {
        let stream_result = match fmt {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|f| f.iter().sum::<f32>() / channels as f32)
                        .collect();
                    on_mono(&mono, sample_rate);
                },
                |e| eprintln!("[audio] stream error: {}", e),
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|f| {
                            f.iter().map(|&s| s as f32 / 32768.0).sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    on_mono(&mono, sample_rate);
                },
                |e| eprintln!("[audio] stream error: {}", e),
                None,
            ),
            other => {
                eprintln!("[audio] unsupported format: {:?}", other);
                return;
            }
        };

        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[audio] build stream err: {}", e);
                return;
            }
        };
        if let Err(e) = stream.play() {
            eprintln!("[audio] play err: {}", e);
            return;
        }

        while !stop_flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    Ok(())
}

/// Live mic-level preview: emit "mic-level" events with RMS 0..1 at ~15Hz.
/// Returns immediately; consumer signals stop via `stop_flag`.
pub fn run_mic_preview(
    app: AppHandle,
    device_name: Option<String>,
    stop_flag: Arc<AtomicBool>,
) -> Result<()> {
    let device = pick_device(device_name.as_deref())?;
    eprintln!("[mic-preview] device: {}", device.name().unwrap_or_default());

    let last_emit = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app_cb = app.clone();
    let last_emit_cb = last_emit.clone();

    spawn_capture(
        device,
        stop_flag,
        Box::new(move |mono: &[f32], _rate: u32| {
            if mono.is_empty() {
                return;
            }
            let rms: f32 = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
            // Scale to 0..1 with mild log curve so quiet speech is visible
            let level = (rms * 5.0).min(1.0);
            let mut t = last_emit_cb.lock().unwrap();
            if t.elapsed() >= std::time::Duration::from_millis(66) {
                *t = std::time::Instant::now();
                drop(t);
                let _ = app_cb.emit("mic-level", level);
            }
        }),
    )?;
    Ok(())
}

/// Lightweight Soniox API key validation.
/// Opens a WebSocket, sends start config, awaits first message. Closes after.
pub async fn test_api_key(api_key: String) -> Result<()> {
    let (ws_stream, _) = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        connect_async(SONIOX_WS_URL),
    )
    .await
    .context("Connection timed out")?
    .context("Failed to connect to Soniox")?;

    let (mut tx, mut rx) = ws_stream.split();

    let start = serde_json::json!({
        "api_key": api_key,
        "model": "stt-rt-v4",
        "audio_format": "pcm_s16le",
        "sample_rate": TARGET_SAMPLE_RATE,
        "num_channels": 1,
    });
    tx.send(Message::Text(start.to_string()))
        .await
        .context("Failed to send config")?;

    // Read first non-empty response (or wait 3s)
    let recv = async {
        while let Some(msg) = rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(parsed) = serde_json::from_str::<SonioxMessage>(&text) {
                        if let Some(err) = parsed.error {
                            return Err(anyhow!("Soniox: {}", err));
                        }
                        // No error field — server accepted handshake
                        return Ok(());
                    }
                    return Ok(());
                }
                Ok(Message::Close(frame)) => {
                    if let Some(f) = frame {
                        return Err(anyhow!("Server closed: {}", f.reason));
                    }
                    return Err(anyhow!("Server closed connection"));
                }
                Err(e) => return Err(anyhow!("WS error: {}", e)),
                _ => {}
            }
        }
        // Stream ended without any message
        Ok(())
    };

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), recv).await;
    let _ = tx.send(Message::Close(None)).await;
    match result {
        Ok(r) => r,
        Err(_) => Ok(()), // no response within 5s → assume OK (server is waiting for audio)
    }
}

pub async fn run_capture(
    app: AppHandle,
    api_key: String,
    device_name: Option<String>,
    language: Option<String>,
    target_language: Option<String>,
    stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let device = pick_device(device_name.as_deref())?;
    eprintln!("[audio] device: {}", device.name().unwrap_or_default());

    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(256);
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Accumulator state lives in the closure
    let buf: Arc<std::sync::Mutex<Vec<f32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tx_clone = audio_tx.clone();
    let stop_thread = stop_flag.clone();
    let buf_cb = buf.clone();
    let chunk_ms: u32 = 100;

    spawn_capture(
        device,
        stop_flag.clone(),
        Box::new(move |mono: &[f32], sample_rate: u32| {
            if stop_thread.load(Ordering::Relaxed) {
                return;
            }
            let chunk_frames = (sample_rate as usize * chunk_ms as usize) / 1000;
            let mut b = buf_cb.lock().unwrap();
            b.extend_from_slice(mono);
            while b.len() >= chunk_frames {
                let chunk: Vec<f32> = b.drain(..chunk_frames).collect();
                let resampled = resample(&chunk, sample_rate);
                let pcm = to_pcm_bytes(&resampled);
                if tx_clone.blocking_send(pcm).is_err() {
                    stop_thread.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }),
    )?;

    // Connect WS
    eprintln!("[ws] connecting...");
    let (ws_stream, _) = connect_async(SONIOX_WS_URL)
        .await
        .context("Failed to connect to Soniox. Check internet/key.")?;
    eprintln!("[ws] connected");

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let mut start_msg = serde_json::json!({
        "api_key": api_key,
        "model": "stt-rt-v4",
        "audio_format": "pcm_s16le",
        "sample_rate": TARGET_SAMPLE_RATE,
        "num_channels": 1,
        "include_word_timing": true
    });
    if let Some(lang) = language.as_ref() {
        if !lang.is_empty() && lang != "auto" {
            start_msg["language_hints"] = serde_json::Value::Array(vec![
                serde_json::Value::String(lang.clone()),
            ]);
        }
    }
    let has_translation = target_language
        .as_ref()
        .map(|t| !t.is_empty() && t != "none")
        .unwrap_or(false);
    if has_translation {
        let tgt = target_language.as_ref().unwrap();
        start_msg["translation"] = serde_json::json!({
            "type": "one_way",
            "target_language": tgt
        });
        eprintln!("[ws] translation: one_way → {}", tgt);
    }
    ws_tx
        .send(Message::Text(start_msg.to_string()))
        .await
        .context("Failed to send start config")?;
    eprintln!("[ws] start config sent");

    let (ws_done_tx, mut ws_done_rx) = tokio::sync::oneshot::channel::<()>();

    let app_recv = app.clone();
    let has_target_clone = has_translation;
    tokio::spawn(async move {
        let mut source_stable = String::new();
        let mut target_stable = String::new();
        // Token classifier — bucket: ("source"|"target")
        fn bucket_of(status: &Option<String>) -> &'static str {
            match status.as_deref() {
                Some("translation") => "target",
                _ => "source", // "original", "none", or missing
            }
        }
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(parsed) = serde_json::from_str::<SonioxMessage>(&text) {
                        if let Some(err) = parsed.error {
                            let code = parsed.error_code.unwrap_or(0);
                            let display = if code != 0 {
                                format!("Soniox [{}]: {}", code, err)
                            } else {
                                format!("Soniox: {}", err)
                            };
                            eprintln!("[ws] server error: {}", display);
                            let _ = app_recv.emit("transcript-error", display);
                            break;
                        }
                        if let Some(tokens) = parsed.tokens {
                            let meaningful: Vec<&SonioxToken> = tokens
                                .iter()
                                .filter(|t| !t.text.is_empty() && !t.text.starts_with('<'))
                                .collect();
                            if meaningful.is_empty() {
                                continue;
                            }
                            let mut src_final = String::new();
                            let mut src_live = String::new();
                            let mut tgt_final = String::new();
                            let mut tgt_live = String::new();
                            for t in &meaningful {
                                let b = bucket_of(&t.translation_status);
                                let is_final = t.is_final == Some(true);
                                match (b, is_final) {
                                    ("source", true) => src_final.push_str(&t.text),
                                    ("source", false) => src_live.push_str(&t.text),
                                    ("target", true) => tgt_final.push_str(&t.text),
                                    ("target", false) => tgt_live.push_str(&t.text),
                                    _ => {}
                                }
                            }
                            // Append finals to stable buffers with rolling window
                            if !src_final.is_empty() {
                                source_stable.push_str(&src_final);
                                let words: Vec<&str> = source_stable.split_whitespace().collect();
                                if words.len() > 120 {
                                    source_stable = words[words.len() - 80..].join(" ") + " ";
                                }
                            }
                            if !tgt_final.is_empty() {
                                target_stable.push_str(&tgt_final);
                                let words: Vec<&str> = target_stable.split_whitespace().collect();
                                if words.len() > 120 {
                                    target_stable = words[words.len() - 80..].join(" ") + " ";
                                }
                            }
                            let _ = app_recv.emit(
                                "transcript",
                                TranscriptEvent {
                                    source_stable: source_stable.trim_end().to_string(),
                                    source_live: src_live,
                                    target_stable: target_stable.trim_end().to_string(),
                                    target_live: tgt_live,
                                    has_target: has_target_clone,
                                },
                            );
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        eprintln!("[ws] recv loop ended");
        let _ = ws_done_tx.send(());
    });

    let mut stop_rx = stop_rx;
    loop {
        tokio::select! {
            _ = &mut stop_rx => {
                eprintln!("[ws] user stopped");
                stop_flag.store(true, Ordering::Relaxed);
                let _ = ws_tx.send(Message::Text(
                    serde_json::json!({"type": "finalize"}).to_string()
                )).await;
                let _ = ws_tx.send(Message::Close(None)).await;
                break;
            }
            _ = &mut ws_done_rx => {
                stop_flag.store(true, Ordering::Relaxed);
                break;
            }
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(pcm) => {
                        if let Err(e) = ws_tx.send(Message::Binary(pcm)).await {
                            eprintln!("[ws] send err: {}", e);
                            stop_flag.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}
