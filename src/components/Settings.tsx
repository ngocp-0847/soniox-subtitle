import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "./Settings.css";

interface Props {
  onClose: () => void;
  onSettingsChanged: () => void;
}

interface AudioDevice {
  name: string;
  is_default: boolean;
  is_advanced: boolean;
}

interface SettingsState {
  api_key: string;
  device_name: string;
  language: string;
  opacity: number;       // 0..1
  font_size: number;     // px
  max_words: number;
  auto_start: boolean;
  shortcut: string;
  show_advanced: boolean;
}

const DEFAULTS: SettingsState = {
  api_key: "",
  device_name: "Default",
  language: "auto",
  opacity: 0.92,
  font_size: 18,
  max_words: 80,
  auto_start: false,
  shortcut: "CommandOrControl+Shift+S",
  show_advanced: false,
};

const LANGUAGES: { code: string; label: string }[] = [
  { code: "auto", label: "Auto-detect" },
  { code: "en", label: "English" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "ja", label: "日本語" },
  { code: "ko", label: "한국어" },
  { code: "zh", label: "中文" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "ru", label: "Русский" },
  { code: "pt", label: "Português" },
  { code: "it", label: "Italiano" },
  { code: "id", label: "Indonesia" },
  { code: "th", label: "ไทย" },
  { code: "hi", label: "हिन्दी" },
  { code: "ar", label: "العربية" },
];

export default function Settings({ onClose, onSettingsChanged }: Props) {
  const [s, setS] = useState<SettingsState>(DEFAULTS);
  const [loading, setLoading] = useState(true);
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [showKey, setShowKey] = useState(false);
  const [deviceFilter, setDeviceFilter] = useState("");
  const [testStatus, setTestStatus] = useState<"idle" | "testing" | "ok" | "fail">("idle");
  const [testMsg, setTestMsg] = useState("");
  const [saved, setSaved] = useState(false);
  const [micLevel, setMicLevel] = useState(0);
  const [recordingShortcut, setRecordingShortcut] = useState(false);

  const update = <K extends keyof SettingsState>(k: K, v: SettingsState[K]) =>
    setS((prev) => ({ ...prev, [k]: v }));

  // Load settings + devices once
  useEffect(() => {
    (async () => {
      try {
        const stored = await invoke<Partial<SettingsState>>("get_settings");
        setS({ ...DEFAULTS, ...stored });
      } catch (e) {
        console.error(e);
      }
      try {
        const list = await invoke<AudioDevice[]>("list_audio_devices");
        setDevices(list);
      } catch (e) {
        console.error(e);
      }
      setLoading(false);
    })();
  }, []);

  // Mic level subscription + start preview when device changes
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    (async () => {
      unlisten = await listen<number>("mic-level", (e) => setMicLevel(e.payload));
      try {
        await invoke("start_mic_preview", {
          deviceName: s.device_name === "Default" ? null : s.device_name,
        });
      } catch (e) {
        console.error("[mic preview]", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
      invoke("stop_mic_preview").catch(() => {});
    };
  }, [s.device_name]);

  // Capture global shortcut keypress when "recording" mode active
  useEffect(() => {
    if (!recordingShortcut) return;
    const handler = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const parts: string[] = [];
      if (e.ctrlKey || e.metaKey) parts.push("CommandOrControl");
      if (e.altKey) parts.push("Alt");
      if (e.shiftKey) parts.push("Shift");
      // Ignore lone modifier keys
      const k = e.key;
      if (k === "Control" || k === "Shift" || k === "Alt" || k === "Meta") return;
      let keyName = k.length === 1 ? k.toUpperCase() : k;
      if (keyName === "Escape") {
        setRecordingShortcut(false);
        return;
      }
      if (keyName === " ") keyName = "Space";
      parts.push(keyName);
      update("shortcut", parts.join("+"));
      setRecordingShortcut(false);
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [recordingShortcut]);

  const filteredDevices = useMemo(() => {
    const term = deviceFilter.trim().toLowerCase();
    return devices.filter((d) => {
      if (!s.show_advanced && d.is_advanced) return false;
      if (term && !d.name.toLowerCase().includes(term)) return false;
      return true;
    });
  }, [devices, s.show_advanced, deviceFilter]);

  const handleTest = async () => {
    if (!s.api_key.trim()) return;
    setTestStatus("testing");
    setTestMsg("");
    try {
      await invoke<boolean>("test_api_key", { key: s.api_key });
      setTestStatus("ok");
      setTestMsg("Connected to Soniox");
    } catch (e) {
      setTestStatus("fail");
      setTestMsg(String(e));
    }
  };

  const handleSave = async () => {
    await invoke("set_settings", {
      values: {
        api_key: s.api_key,
        device_name: s.device_name,
        language: s.language,
        opacity: s.opacity,
        font_size: s.font_size,
        max_words: s.max_words,
        auto_start: s.auto_start,
        shortcut: s.shortcut,
        show_advanced: s.show_advanced,
      },
    });
    // Re-register shortcut
    try {
      await invoke("register_shortcut", { accel: s.shortcut });
    } catch (e) {
      console.error("[shortcut]", e);
    }
    setSaved(true);
    onSettingsChanged();
    setTimeout(() => setSaved(false), 1800);
  };

  const handleReset = async () => {
    if (!window.confirm("Reset all settings to defaults? Your API key will be cleared.")) return;
    await invoke("reset_settings");
    setS(DEFAULTS);
    onSettingsChanged();
  };

  if (loading) {
    return (
      <div className="settings">
        <div className="settings-header">
          <h2>Settings</h2>
          <button className="btn-icon-settings" onClick={onClose}>✕</button>
        </div>
        <div className="loading">Loading...</div>
      </div>
    );
  }

  return (
    <div className="settings">
      <div className="settings-header">
        <h2>Settings</h2>
        <button className="btn-icon-settings" onClick={onClose}>✕</button>
      </div>

      <div className="settings-body">
        {/* API KEY */}
        <div className="settings-section">
          <label className="label">Soniox API Key</label>
          <p className="hint">
            Get yours at <a href="https://console.soniox.com" target="_blank" rel="noreferrer">console.soniox.com</a> → API Keys
          </p>
          <div className="input-with-action">
            <input
              type={showKey ? "text" : "password"}
              placeholder="sk-..."
              value={s.api_key}
              onChange={(e) => update("api_key", e.target.value)}
            />
            <button
              className="btn-inline"
              onClick={() => setShowKey((v) => !v)}
              title={showKey ? "Hide" : "Show"}
            >
              {showKey ? "🙈" : "👁"}
            </button>
            <button
              className="btn-inline"
              onClick={handleTest}
              disabled={!s.api_key.trim() || testStatus === "testing"}
              title="Test connection"
            >
              {testStatus === "testing" ? "…" : "Test"}
            </button>
          </div>
          {testStatus === "ok" && <p className="status-ok">✓ {testMsg}</p>}
          {testStatus === "fail" && <p className="status-fail">✗ {testMsg}</p>}
        </div>

        {/* AUDIO */}
        <div className="settings-section">
          <label className="label">Input Device</label>
          <input
            type="text"
            className="filter-input"
            placeholder="Filter devices..."
            value={deviceFilter}
            onChange={(e) => setDeviceFilter(e.target.value)}
          />
          <ul className="device-list">
            {filteredDevices.map((d) => {
              const selected = (d.name === "Default" && s.device_name === "Default") || d.name === s.device_name;
              return (
                <li
                  key={d.name}
                  className={`device-item ${selected ? "selected" : ""}`}
                  onClick={() => update("device_name", d.name)}
                >
                  <span className="radio">{selected ? "●" : "○"}</span>
                  <span className="device-name" title={d.name}>{d.name}</span>
                  {d.is_default && d.name !== "Default" && <span className="badge">default</span>}
                  {d.is_advanced && <span className="badge badge-adv">ALSA</span>}
                </li>
              );
            })}
            {filteredDevices.length === 0 && (
              <li className="device-item empty">No devices match filter</li>
            )}
          </ul>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={s.show_advanced}
              onChange={(e) => update("show_advanced", e.target.checked)}
            />
            <span>Show ALSA advanced devices</span>
          </label>

          <div className="level-meter-wrap">
            <span className="level-label">Mic level</span>
            <div className="level-meter">
              <div className="level-fill" style={{ width: `${Math.min(100, micLevel * 100)}%` }} />
            </div>
          </div>
        </div>

        {/* LANGUAGE */}
        <div className="settings-section">
          <label className="label">Language</label>
          <select
            className="select"
            value={s.language}
            onChange={(e) => update("language", e.target.value)}
          >
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>{l.label}</option>
            ))}
          </select>
          <p className="hint">Hint for Soniox model. Auto-detect works for most cases.</p>
        </div>

        {/* APPEARANCE */}
        <div className="settings-section">
          <label className="label">Appearance</label>

          <div className="slider-row">
            <span className="slider-label">Background opacity</span>
            <span className="slider-val">{Math.round(s.opacity * 100)}%</span>
          </div>
          <input
            type="range"
            min={0}
            max={100}
            step={1}
            value={Math.round(s.opacity * 100)}
            onChange={(e) => update("opacity", Number(e.target.value) / 100)}
          />
          <p className="hint">0% = fully transparent overlay (perfect for video viewing)</p>

          <div className="slider-row">
            <span className="slider-label">Font size</span>
            <span className="slider-val">{s.font_size}px</span>
          </div>
          <input
            type="range"
            min={12}
            max={32}
            step={1}
            value={s.font_size}
            onChange={(e) => update("font_size", Number(e.target.value))}
          />

          <div className="slider-row">
            <span className="slider-label">Max words on screen</span>
            <span className="slider-val">{s.max_words}</span>
          </div>
          <input
            type="range"
            min={40}
            max={200}
            step={10}
            value={s.max_words}
            onChange={(e) => update("max_words", Number(e.target.value))}
          />
        </div>

        {/* BEHAVIOR */}
        <div className="settings-section">
          <label className="label">Behavior</label>

          <label className="checkbox">
            <input
              type="checkbox"
              checked={s.auto_start}
              onChange={(e) => update("auto_start", e.target.checked)}
            />
            <span>Start recording when app opens</span>
          </label>

          <div className="slider-row">
            <span className="slider-label">Global shortcut</span>
          </div>
          <div className="input-with-action">
            <input
              type="text"
              readOnly
              value={s.shortcut}
              onClick={() => setRecordingShortcut(true)}
              placeholder={recordingShortcut ? "Press keys..." : "Click to set"}
              className={recordingShortcut ? "recording-shortcut" : ""}
            />
            <button className="btn-inline" onClick={() => setRecordingShortcut(true)}>
              {recordingShortcut ? "Press..." : "Set"}
            </button>
          </div>
          <p className="hint">Press Escape to cancel</p>
        </div>

        <div className="settings-actions">
          <button className="btn-secondary" onClick={handleReset}>Reset</button>
          <button className="btn-save" onClick={handleSave}>
            {saved ? "✓ Saved" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
