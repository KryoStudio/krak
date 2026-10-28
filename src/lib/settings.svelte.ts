import { api, errorCode, native, type Settings } from './api';

const defaults: Settings = {
  hotkey: 'F9',
  bufferSeconds: 30,
  fps: 60,
  quality: 'high',
  clipsDir: 'C:\\Users\\you\\Videos\\Kryo Shard',
  language: null,
  accent: 'glacier',
  sidebarCollapsed: false,
  launchAtStartup: false,
  closeToTray: true,
  microphone: false,
  captureSound: true,
  overlay: true,
};

// Browser preview only: in the app, settings live in the backend.
const STORAGE_KEY = 'kryo-shard.settings';

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
