import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Settings from "./components/Settings";
import "./styles/App.css";

interface TranscriptEvent {
  text: string;
  is_final: boolean;
}

export default function App() {
  const [recording, setRecording] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [transcript, setTranscript] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [alwaysOnTop, setAlwaysOnTop] = useState(true);
  const transcriptRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Auto enable always-on-top at start
    invoke("set_always_on_top", { onTop: true });

    const unlisten1 = listen<TranscriptEvent>("transcript", (e) => {
      setTranscript(e.payload.text);
      setError(null);
    });

    const unlisten2 = listen<string>("transcript-error", (e) => {
      setError(e.payload);
      setRecording(false);
    });

    const unlisten3 = listen<boolean>("recording-state", (e) => {
      setRecording(e.payload);
    });

    return () => {
      unlisten1.then((f) => f());
      unlisten2.then((f) => f());
      unlisten3.then((f) => f());
    };
  }, []);

  useEffect(() => {
    // Auto-scroll transcript
    if (transcriptRef.current) {
      transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
    }
  }, [transcript]);

  const toggleRecording = async () => {
    setError(null);
    try {
      if (recording) {
        await invoke("stop_recording");
      } else {
        setTranscript("");
        await invoke("start_recording");
      }
    } catch (e: unknown) {
      setError(String(e));
    }
  };

  const toggleAlwaysOnTop = async () => {
    const next = !alwaysOnTop;
    setAlwaysOnTop(next);
    await invoke("set_always_on_top", { onTop: next });
  };

  const clearTranscript = () => setTranscript("");

  return (
    <div className={`app ${recording ? "is-recording" : ""}`}>
      {/* Draggable header */}
      <div
        className="titlebar"
        data-tauri-drag-region
        onMouseDown={async (e) => {
          // Only drag on left click, not on buttons
          if (e.button === 0 && (e.target as HTMLElement).closest('.titlebar-actions') === null) {
            await getCurrentWindow().startDragging();
          }
        }}
      >
        <div className="titlebar-left">
          <div className={`rec-dot ${recording ? "active" : ""}`} />
          <span className="app-title">Realtime Subtitles</span>
        </div>
        <div className="titlebar-actions">
          <button
            className={`btn-icon ${alwaysOnTop ? "active" : ""}`}
            onClick={toggleAlwaysOnTop}
            title={alwaysOnTop ? "Disable always-on-top" : "Enable always-on-top"}
          >📌</button>
          <button className="btn-icon" onClick={clearTranscript} title="Clear">🗑️</button>
          <button
            className="btn-icon"
            onClick={() => setShowSettings(!showSettings)}
            title="Settings"
          >⚙️</button>
        </div>
      </div>

      {showSettings ? (
        <Settings onClose={() => setShowSettings(false)} />
      ) : (
        <>
          {/* Transcript area */}
          <div className="transcript-area" ref={transcriptRef}>
            {transcript ? (
              <p className="transcript-text">{transcript}</p>
            ) : (
              <p className="transcript-placeholder">
                {recording ? "Listening..." : "Press Start to begin live transcription"}
              </p>
            )}
          </div>

          {error && (
            <div className="error-bar">⚠️ {error}</div>
          )}

          {/* Controls */}
          <div className="controls">
            <button
              className={`btn-record ${recording ? "recording" : ""}`}
              onClick={toggleRecording}
            >
              {recording ? "⏹ Stop" : "🎙 Start"}
            </button>
          </div>
        </>
      )}
    </div>
  );
}
