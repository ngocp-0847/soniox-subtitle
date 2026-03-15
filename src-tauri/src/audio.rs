use anyhow::{Context, Result};
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
}

#[derive(Debug, Deserialize)]
struct SonioxToken {
    text: String,
    is_final: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
pub struct TranscriptEvent {
    pub text: String,
    pub is_final: bool,
}

pub fn list_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut devices = vec!["Default Microphone".to_string()];
    if let Ok(input_devices) = host.input_devices() {
        for device in input_devices {
            if let Ok(name) = device.name() {
                devices.push(name);
            }
        }
    }
    Ok(devices)
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

pub async fn run_capture(
    app: AppHandle,
    api_key: String,
    stop_rx: oneshot::Receiver<()>,
) -> Result<()> {
    // channel: blocking audio thread → async WS sender
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(256);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = stop_flag.clone();

    // Blocking thread for CPAL capture
    std::thread::spawn(move || {
        let host = cpal::default_host();
        eprintln!("[audio] host: {}", host.id().name());

        let device = match host.default_input_device() {
            Some(d) => d,
            None => {
                eprintln!("[audio] ERROR: No input device");
                return;
            }
        };
        eprintln!("[audio] device: {}", device.name().unwrap_or_default());

        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[audio] Config error: {}", e);
                return;
            }
        };

        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        eprintln!(
            "[audio] format={:?} ch={} rate={}",
            config.sample_format(),
            channels,
            sample_rate
        );

        // Accumulate raw f32 mono samples, drain every 100ms worth
        let chunk_frames = (sample_rate as usize * 100) / 1000;
        eprintln!("[audio] chunk_frames={} (100ms)", chunk_frames);

        let buf = Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));

        // Closure: convert input → mono f32 → push to buf → drain & send
        let make_cb = {
            let buf = buf.clone();
            let tx = audio_tx.clone();
            let stop = stop_flag_thread.clone();
            move || {
                let buf = buf.clone();
                let tx = tx.clone();
                let stop = stop.clone();
                move |mono_chunk: Vec<f32>| {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    // Log RMS level occasionally
                    let rms: f32 = (mono_chunk.iter().map(|s| s * s).sum::<f32>()
                        / mono_chunk.len() as f32)
                        .sqrt();
                    if rms > 0.001 {
                        eprintln!("[audio] rms={:.4} len={}", rms, mono_chunk.len());
                    }
                    let mut b = buf.lock().unwrap();
                    b.extend_from_slice(&mono_chunk);
                    while b.len() >= chunk_frames {
                        let chunk: Vec<f32> = b.drain(..chunk_frames).collect();
                        let resampled = resample(&chunk, sample_rate);
                        let pcm = to_pcm_bytes(&resampled);
                        if tx.blocking_send(pcm).is_err() {
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            }
        };

        let stream_result = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let mut cb = make_cb();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        let mono: Vec<f32> = data
                            .chunks(channels)
                            .map(|f| f.iter().sum::<f32>() / channels as f32)
                            .collect();
                        cb(mono);
                    },
                    |e| eprintln!("[audio] stream error: {}", e),
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let mut cb = make_cb();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        let mono: Vec<f32> = data
                            .chunks(channels)
                            .map(|f| {
                                f.iter().map(|&s| s as f32 / 32768.0).sum::<f32>()
                                    / channels as f32
                            })
                            .collect();
                        cb(mono);
                    },
                    |e| eprintln!("[audio] stream error: {}", e),
                    None,
                )
            }
            fmt => {
                eprintln!("[audio] Unsupported format: {:?}", fmt);
                return;
            }
        };

        let stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[audio] Build stream error: {}", e);
                return;
            }
        };

        if let Err(e) = stream.play() {
            eprintln!("[audio] Play error: {}", e);
            return;
        }

        eprintln!("[audio] Capture running...");
        while !stop_flag_thread.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        eprintln!("[audio] Capture stopped");
        // stream drops here
    });

    // Connect WebSocket
    eprintln!("[ws] Connecting...");
    let (ws_stream, _) = connect_async(SONIOX_WS_URL)
        .await
        .context("Failed to connect to Soniox. Check API key and internet.")?;
    eprintln!("[ws] Connected");

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Send start config
    let start_msg = serde_json::json!({
        "api_key": api_key,
        "model": "stt-rt-v4",
        "audio_format": "pcm_s16le",
        "sample_rate": TARGET_SAMPLE_RATE,
        "num_channels": 1,
        "include_word_timing": true
    });
    ws_tx
        .send(Message::Text(start_msg.to_string()))
        .await
        .context("Failed to send start config")?;
    eprintln!("[ws] Start config sent");

    // Shutdown channel: recv task → send loop
    let (ws_done_tx, mut ws_done_rx) = tokio::sync::oneshot::channel::<()>();

    // Receive transcripts
    let app_recv = app.clone();
    tokio::spawn(async move {
        let mut stable_text = String::new();
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(parsed) = serde_json::from_str::<SonioxMessage>(&text) {
                        if let Some(err) = parsed.error {
                            eprintln!("[ws] server error: {}", err);
                            let _ = app_recv.emit("transcript-error", err);
                            break;
                        }
                        if let Some(tokens) = parsed.tokens {
                            let meaningful: Vec<&SonioxToken> = tokens
                                .iter()
                                .filter(|t| {
                                    !t.text.is_empty()
                                        && !t.text.starts_with('<')
                                })
                                .collect();

                            if meaningful.is_empty() {
                                continue;
                            }

                            eprintln!(
                                "[ws] tokens: {:?}",
                                meaningful.iter().map(|t| &t.text).collect::<Vec<_>>()
                            );

                            let mut new_text = String::new();
                            let mut has_final = false;
                            for t in &meaningful {
                                if t.is_final == Some(true) {
                                    has_final = true;
                                }
                                new_text.push_str(&t.text);
                            }

                            if has_final {
                                stable_text.push_str(new_text.trim());
                                stable_text.push(' ');
                                let words: Vec<&str> =
                                    stable_text.split_whitespace().collect();
                                if words.len() > 80 {
                                    stable_text =
                                        words[words.len() - 50..].join(" ") + " ";
                                }
                                let _ = app_recv.emit(
                                    "transcript",
                                    TranscriptEvent {
                                        text: stable_text.trim().to_string(),
                                        is_final: true,
                                    },
                                );
                            } else {
                                let _ = app_recv.emit(
                                    "transcript",
                                    TranscriptEvent {
                                        text: format!("{}{}", stable_text, new_text.trim()),
                                        is_final: false,
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        eprintln!("[ws] Recv loop ended");
        let _ = ws_done_tx.send(());
    });

    // Send loop: forward audio → WS
    let mut stop_rx = stop_rx;
    loop {
        tokio::select! {
            _ = &mut stop_rx => {
                eprintln!("[ws] User stopped recording");
                stop_flag.store(true, Ordering::Relaxed);
                let _ = ws_tx.send(Message::Text(
                    serde_json::json!({"type": "finalize"}).to_string()
                )).await;
                let _ = ws_tx.send(Message::Close(None)).await;
                break;
            }
            _ = &mut ws_done_rx => {
                eprintln!("[ws] WS closed by server");
                stop_flag.store(true, Ordering::Relaxed);
                break;
            }
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(pcm) => {
                        if let Err(e) = ws_tx.send(Message::Binary(pcm)).await {
                            eprintln!("[ws] Send error: {}", e);
                            stop_flag.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    None => {
                        eprintln!("[ws] Audio channel closed");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
