import { api, errorCode, native, type Settings } from './api';

const defaults: Settings = {
  hotkey: 'F9',
  recordHotkey: 'Shift+F9',
  screenshotHotkey: 'Ctrl+F9',
  discordPresence: true,
  watermark: false,
  bufferSeconds: 30,
  quality: 'high',
  codec: 'auto',
  resolution: 'native',
  fps: 60,
  bitrate: 40,
  tenBit: true,
  screenFit: 'screen',
  audioBitrate: 192,
  clipsDir: 'C:\\Users\\you\\Videos\\Krak',
  language: null,
  accent: 'glacier',
  sidebarCollapsed: false,
  launchAtStartup: false,
  closeToTray: true,
  microphone: false,
  microphoneDevice: null,
  captureSound: true,
  overlay: true,
  storageLimitGb: null,
  onboarded: false,
};

// Browser preview only: in the app, settings live in the backend.
const STORAGE_KEY = 'krak.settings';

class SettingsStore {
  value = $state<Settings>({ ...defaults });
  /** The UI waits for this so it never flashes the defaults (language, accent, sidebar). */
  loaded = $state(false);
  #pending = 0;

  async load() {
    try {
      if (native) this.value = await api.getSettings();
      else Object.assign(this.value, JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}'));
    } catch (e) {
      console.error(e);
    } finally {
      this.loaded = true;
    }
  }

  /** Applies the patch at once; rolls back and throws the error code if the backend refuses it. */
  async update(patch: Partial<Settings>) {
    const before = { ...this.value };
    Object.assign(this.value, patch);
    if (!native) {
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(this.value));
      } catch {
        // no storage
      }
      return;
    }
    const call = ++this.#pending;
    try {
      const saved = await api.updateSettings(patch);
      // A newer update is in flight: its answer will be the fresher one.
      if (call === this.#pending) this.value = saved;
    } catch (e) {
      Object.assign(this.value, before);
      throw errorCode(e);
    }
  }

  async chooseFolder() {
    const saved = await api.chooseClipsDir().catch((e) => {
      throw errorCode(e);
    });
    if (saved) this.value = saved;
  }
}

export const settings = new SettingsStore();
