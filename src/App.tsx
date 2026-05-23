import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Settings from "./components/Settings";
import "./styles/App.css";

interface TranscriptEvent {
  source_stable: string;
  source_live: string;
  target_stable: string;
  target_live: string;
  has_target: boolean;
}

interface AppSettings {
  opacity?: number;
  font_size?: number;
  max_words?: number;
  auto_start?: boolean;
}

const EMPTY_TRANSCRIPT: TranscriptEvent = {
  source_stable: "",
  source_live: "",
  target_stable: "",
  target_live: "",
  has_target: false,
};

function TranscriptZone({
  stable,
  live,
  maxWords,
}: {
  stable: string;
  live: string;
  maxWords: number;
}) {
  const allWords = stable.split(/\s+/).filter(Boolean);
  const words = allWords.slice(-maxWords);
  const total = words.length;

  return (
    <p className="transcript-text">
      {words.map((w, i) => {
        const distance = total - 1 - i;
        const opacity = Math.max(0.25, 1 - distance * 0.055);
        const scale = Math.max(0.82, 1 - distance * 0.018);
        return (
          <span
            key={`${i}-${w}`}
            className="w stable"
            style={{ opacity, fontSize: `${scale}em` }}
          >
            {w}{" "}
          </span>
        );
      })}
      {live && <span className="w live">{live}</span>}
    </p>
  );
}

export default function App() {
  const [recording, setRecording] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [transcript, setTranscript] = useState<TranscriptEvent>(EMPTY_TRANSCRIPT);
  const [error, setError] = useState<string | null>(null);
  const [alwaysOnTop, setAlwaysOnTop] = useState(false);
  const [maxWords, setMaxWords] = useState(80);
  const alwaysOnTopRef = useRef(false);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const recordingRef = useRef(false);

  const applySettings = useCallback(async () => {
    try {
      const s = await invoke<AppSettings>("get_settings");
      const opacity = typeof s.opacity === "number" ? s.opacity : 0.92;
      const fontSize = typeof s.font_size === "number" ? s.font_size : 18;
      const mw = typeof s.max_words === "number" ? s.max_words : 80;
      document.documentElement.style.setProperty("--app-bg-opacity", String(opacity));
      document.documentElement.style.setProperty("--transcript-font-size", `${fontSize}px`);
      setMaxWords(mw);
    } catch (e) {
      console.error("[settings load]", e);
    }
  }, []);

  const toggleRecording = useCallback(async () => {
    setError(null);
    try {
      if (recordingRef.current) {
        await invoke("stop_recording");
      } else {
        setTranscript(EMPTY_TRANSCRIPT);
        await invoke("start_recording");
      }
    } catch (e: unknown) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    recordingRef.current = recording;
  }, [recording]);

  useEffect(() => {
    (async () => {
      await applySettings();
      // Don't auto-enable always-on-top — start unpinned so user can drag freely.
      try {
        const s = await invoke<AppSettings>("get_settings");
        if (s.auto_start) {
          setTimeout(() => toggleRecording(), 500);
        }
      } catch {}
    })();

    const unlisten1 = listen<TranscriptEvent>("transcript", (e) => {
      setTranscript(e.payload);
      setError(null);
    });
    const unlisten2 = listen<string>("transcript-error", (e) => {
      setError(e.payload);
      setRecording(false);
    });
    const unlisten3 = listen<boolean>("recording-state", (e) => {
      setRecording(e.payload);
    });
    const unlisten4 = listen("shortcut-toggle-recording", () => {
      toggleRecording();
    });

    return () => {
      unlisten1.then((f) => f());
      unlisten2.then((f) => f());
      unlisten3.then((f) => f());
      unlisten4.then((f) => f());
    };
  }, [applySettings, toggleRecording]);

  useEffect(() => {
    if (transcriptRef.current) {
      transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
    }
  }, [transcript]);

  const toggleAlwaysOnTop = async () => {
    const next = !alwaysOnTop;
    setAlwaysOnTop(next);
    alwaysOnTopRef.current = next;
    await invoke("set_always_on_top", { onTop: next });
  };

  const closeApp = async () => {
    await getCurrentWindow().close();
  };

  const clearTranscript = () => setTranscript(EMPTY_TRANSCRIPT);

  const hasContent =
    !!transcript.source_stable.trim() ||
    !!transcript.source_live.trim() ||
    !!transcript.target_stable.trim() ||
    !!transcript.target_live.trim();

  return (
    <div className={`app ${recording ? "is-recording" : ""}`}>
      <div
        className="titlebar"
        onMouseDown={async (e) => {
          if (
            e.button === 0 &&
            !alwaysOnTopRef.current &&
            (e.target as HTMLElement).closest('.titlebar-actions') === null
          ) {
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
          <button
            className="btn-icon btn-close"
            onClick={closeApp}
            title="Close"
          >✕</button>
        </div>
      </div>

      {showSettings ? (
        <Settings
          onClose={() => setShowSettings(false)}
          onSettingsChanged={applySettings}
        />
      ) : (
        <>
          <div className="transcript-area" ref={transcriptRef}>
            {!hasContent ? (
              <p className="transcript-placeholder">
                {recording ? "Listening..." : "Press Start to begin live transcription"}
              </p>
            ) : transcript.has_target ? (
              <>
                <div className="zone source-zone">
                  <span className="zone-label">SOURCE</span>
                  <TranscriptZone
                    stable={transcript.source_stable}
                    live={transcript.source_live}
                    maxWords={maxWords}
                  />
                </div>
                <div className="zone-divider" />
                <div className="zone target-zone">
                  <span className="zone-label">TRANSLATION</span>
                  <TranscriptZone
                    stable={transcript.target_stable}
                    live={transcript.target_live}
                    maxWords={maxWords}
                  />
                </div>
              </>
            ) : (
              <div className="zone single-zone">
                <TranscriptZone
                  stable={transcript.source_stable}
                  live={transcript.source_live}
                  maxWords={maxWords}
                />
              </div>
            )}
          </div>

          {error && <div className="error-bar">⚠️ {error}</div>}

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
