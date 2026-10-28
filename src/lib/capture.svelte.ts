// The replay buffer as the UI sees it. In the browser preview it pretends to be running.
import { api, errorCode, native, type CaptureStatus } from './api';
import { demoShards } from './demo';
import { library } from './library.svelte';
import { settings } from './settings.svelte';
import { announce, showError } from './shard-actions';

const preview: CaptureStatus = {
  state: 'ready',
  encoder: 'nvidia',
  width: 1920,
  height: 1080,
  fps: 60,
  bufferedSeconds: 30,
  memoryBytes: 62_000_000,
  microphone: false,
  error: null,
  audioError: null,
};

class Capture {
  status = $state<CaptureStatus>(native ? { ...preview, state: 'starting', encoder: null, bufferedSeconds: 0, memoryBytes: 0 } : preview);

  async refresh() {
    if (native) this.status = await api.getCaptureStatus();
  }

  /** Throws an error code if the backend refuses. */
  async setPaused(paused: boolean) {
    if (!native) {
      this.status.state = paused ? 'paused' : 'ready';
      return;
    }
    this.status = await api.setCapturePaused(paused).catch((e) => {
      throw errorCode(e);
    });
  }

  get live() {
    return this.status.state === 'ready';
  }
}

export const capture = new Capture();

/** Same as the hotkey. The result arrives as a `shard-saved` or `capture-failed` event. */
export async function saveReplay() {
  if (native) {
    await api.saveReplay().catch((e) => showError(errorCode(e)));
    return;
  }
  // Browser preview: pretend a demo shard was just captured.
  const pool = library.shards.length ? library.shards : demoShards();
  const base = pool[Math.floor(Math.random() * pool.length)];
  const id = `${base.game ? `${base.game}/` : ''}Preview ${Date.now()}.mp4`;
  announce({ ...base, id, title: null, createdAt: Date.now(), favorite: false, duration: settings.value.bufferSeconds });
}

/** Rough RAM the buffer will take: encoded video plus raw audio (48 kHz stereo 16-bit per source). */
export function estimateMemory(seconds: number, fps: number, quality: string, microphone: boolean, height = 1080): number {
  const bpp = quality === 'balanced' ? 0.07 : quality === 'ultra' ? 0.17 : 0.11;
  const h = quality === 'balanced' ? Math.min(height, 1080) : quality === 'high' ? Math.min(height, 1440) : height;
  const w = Math.round((h * 16) / 9);
  const videoBps = Math.max(3_000_000, w * h * fps * bpp);
  const audioBps = 48_000 * 2 * 2 * (microphone ? 2 : 1);
  return seconds * (videoBps / 8 + audioBps);
}
