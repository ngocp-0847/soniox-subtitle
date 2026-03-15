import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Settings.css";

interface Props {
  onClose: () => void;
}

export default function Settings({ onClose }: Props) {
  const [apiKey, setApiKey] = useState("");
  const [saved, setSaved] = useState(false);
  const [loading, setLoading] = useState(true);
  const [devices, setDevices] = useState<string[]>([]);

  useEffect(() => {
    invoke<string | null>("get_api_key").then((key) => {
      if (key) setApiKey(key);
      setLoading(false);
    });
    invoke<string[]>("list_audio_devices").then(setDevices).catch(console.error);
  }, []);

  const handleSave = async () => {
    await invoke("set_api_key", { key: apiKey });
    setSaved(true);
    setTimeout(() => { setSaved(false); onClose(); }, 1200);
  };

  return (
    <div className="settings">
      <div className="settings-header">
        <h2>Settings</h2>
        <button className="btn-icon-settings" onClick={onClose}>✕</button>
      </div>

      <div className="settings-body">
        <div className="settings-section">
          <label className="label">Soniox API Key</label>
          <p className="hint">Get yours at <a href="https://console.soniox.com" target="_blank" rel="noreferrer">console.soniox.com</a> → API Keys</p>
          {loading ? (
            <div className="loading">Loading...</div>
          ) : (
            <input
              type="password"
              placeholder="sk-..."
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
            />
          )}
        </div>

        {devices.length > 0 && (
          <div className="settings-section">
            <label className="label">Input Devices</label>
            <ul className="device-list">
              {devices.map((d, i) => (
                <li key={i} className="device-item">{d}</li>
              ))}
            </ul>
            <p className="hint">Uses default input device automatically</p>
          </div>
        )}

        <div className="settings-actions">
          <button className="btn-save" onClick={handleSave} disabled={!apiKey.trim()}>
            {saved ? "✓ Saved!" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
