// The replay buffer and the encoder as the UI sees them. In the browser preview it pretends.
import {
  api,
  errorCode,
  native,
  type CaptureStatus,
  type EncoderInfo,
  type Microphone,
  type ResolvedVideo,
  type Settings,
  type StorageInfo,
} from './api';
import { demoShards } from './demo';
import { library } from './library.svelte';
import { settings } from './settings.svelte';
import { t } from './i18n.svelte';
import { announce, showError } from './shard-actions';
import { toast } from './ui.svelte';

const preview: CaptureStatus = {
  state: 'ready',
  encoder: 'nvidia',
  width: 1920,
  height: 1080,
  fps: 60,
  bufferedSeconds: 30,
  memoryBytes: 150_000_000,
  microphone: false,
  error: null,
  audioError: null,
  codec: 'h264',
  tenBit: false,
  bitrate: 40_000_000,
  backend: 'nvenc',
  recording: null,
};

const previewEncoder: EncoderInfo = {
  gpu: 'NVIDIA GeForce RTX 4070',
  vendor: 'nvidia',
  nvenc: true,
  codecs: { h264: true, hevc: true, av1: true },
  tenBit: { hevc: true, av1: true },
  decoders: { hevc: true, av1: true },
  refreshRate: 144,
  width: 2560,
  height: 1440,
};

class Capture {
  status = $state<CaptureStatus>(
    native ? { ...preview, state: 'starting', encoder: null, bufferedSeconds: 0, memoryBytes: 0, codec: null, backend: null } : preview,
  );
  /** null until known (or if this backend can't tell). */
  encoder = $state<EncoderInfo | null>(native ? null : previewEncoder);

  async refresh() {
    if (native) this.status = await api.getCaptureStatus();
  }

  async loadEncoder() {
    if (native && !this.encoder) this.encoder = await api.getEncoderInfo().catch(() => null);
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

  #previewTimer: ReturnType<typeof setInterval> | undefined;

  /** Starts or stops continuous recording. */
  async toggleRecording() {
    const recording = !!this.status.recording;
    if (!native) {
      // Browser preview: a recording that just counts.
      clearInterval(this.#previewTimer);
      if (recording) {
        this.status.recording = null;
        toast({ kind: 'success', title: t('record.saved') });
      } else {
        this.status.recording = { seconds: 0, bytes: 0 };
        this.#previewTimer = setInterval(() => {
          if (!this.status.recording) return;
          this.status.recording.seconds += 1;
          this.status.recording.bytes += 5_000_000;
        }, 1000);
      }
      return;
    }
    try {
      this.status = recording ? await api.stopRecording() : await api.startRecording();
    } catch (e) {
      const code = errorCode(e);
      // An engine without continuous recording yet.
      if (code.includes('not found')) toast({ kind: 'info', title: t('record.soon') });
      else showError(code);
    }
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

// What a set of settings would record ---------------------------------------------------

const even = (n: number) => Math.max(2, Math.round(n / 2) * 2);

/** Local stand-in for `resolve_video` (browser preview, or a backend that can't answer yet). */
function estimate(s: Settings, info: EncoderInfo): ResolvedVideo {
  const presetCap = { standard: 1080, high: 1440, premium: Infinity, custom: Infinity }[s.quality];
  const userCap = s.resolution === 'native' ? Infinity : Number(s.resolution);
  const short = Math.min(info.width, info.height);
  const scale = Math.min(1, Math.min(presetCap, s.quality === 'custom' ? userCap : Infinity) / short);
  const width = even(info.width * scale);
  const height = even(info.height * scale);
  const fps = Math.min(s.fps, info.refreshRate || s.fps);

  const best = info.codecs.hevc && info.decoders.hevc ? 'hevc' : 'h264';
  const codec = s.quality === 'premium' ? best : s.quality === 'custom' ? (s.codec === 'auto' ? best : s.codec) : 'h264';
  const tenBit = codec !== 'h264' && (s.quality === 'premium' || (s.quality === 'custom' && s.tenBit)) && info.tenBit[codec];

  // Mbit/s at 1080p60, scaled with pixels × frame rate.
  const base = { standard: 20, high: 40, premium: codec === 'h264' ? 60 : 50, custom: 0 }[s.quality];
  const bitrate =
    s.quality === 'custom' ? s.bitrate * 1e6 : (base * 1e6 * (width * height * fps)) / (1920 * 1080 * 60);
  return { backend: info.nvenc ? 'nvenc' : 'mediafoundation', codec, tenBit, width, height, fps, bitrate };
}

/** What the engine would use with the current settings plus `patch`. */
export async function resolveVideo(patch: Partial<Settings> = {}): Promise<ResolvedVideo | null> {
  const s = { ...settings.value, ...patch };
  if (native) {
    const resolved = await api.resolveVideo(patch).catch(() => null);
    if (resolved) return resolved;
  }
  const info = capture.encoder ?? (native ? null : previewEncoder);
  return info ? estimate(s, info) : null;
}

/** "12:34" or "1:02:03" */
export function formatClock(seconds: number): string {
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
}

const codecNames = { h264: 'H.264', hevc: 'HEVC', av1: 'AV1' } as const;

/** "HEVC 10 bits · 1080p · 120 i/s" */
export function videoLabel(r: ResolvedVideo, bits: string, fpsUnit: string): string {
  const codec = `${codecNames[r.codec]}${r.tenBit ? ` ${bits}` : ''}`;
  return `${codec} · ${r.height}p · ${r.fps} ${fpsUnit}`;
}

export const codecName = (codec: keyof typeof codecNames) => codecNames[codec];

/** RAM the replay takes: encoded video plus raw audio (48 kHz stereo 16-bit per source). */
export function memoryFor(bitrate: number, seconds: number, microphone: boolean): number {
  return seconds * (bitrate / 8 + 48_000 * 2 * 2 * (microphone ? 2 : 1));
}

export async function listMicrophones(): Promise<Microphone[]> {
  if (!native) return [{ id: 'default', name: 'Microphone (Realtek Audio)', isDefault: true }];
  return api.listMicrophones().catch(() => []);
}

export async function storageInfo(): Promise<StorageInfo | null> {
  if (!native) {
    const used = library.shards.reduce((sum, s) => sum + s.size, 0);
    return { usedBytes: used, clipCount: library.shards.length, freeBytes: 412_000_000_000 };
  }
  return api.getStorageInfo().catch(() => null);
}
