// Typed bridge to the Rust backend. Outside Tauri (plain browser), `native` is false
// and the stores fall back to demo data.
import { convertFileSrc, invoke, isTauri } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export const native = isTauri();

export type Quality = 'standard' | 'high' | 'premium' | 'custom';
export type Codec = 'h264' | 'hevc' | 'av1';
export type Backend = 'nvenc' | 'mediafoundation' | 'software';

export interface Settings {
  hotkey: string;
  /** Starts / stops continuous recording. */
  recordHotkey: string;
  /** Saves the current frame as a PNG. */
  screenshotHotkey: string;
  /** Show what you play and clip on your Discord profile. */
  discordPresence: boolean;
  /** Exports carry a small "by krak" badge (off by default: no watermark unless asked). */
  watermark: boolean;
  bufferSeconds: number;
  quality: Quality;
  /** 'auto' lets the preset decide. */
  codec: 'auto' | Codec;
  /** Cap on the short side; never upscales. */
  resolution: 'native' | '1440' | '1080' | '720';
  /** 30 | 60 | 120 | 144, capped to the monitor refresh rate by the engine. */
  fps: number;
  /** A game in its own resolution (stretched 4:3...): as the screen shows it, always stretched,
   *  or as it is. */
  screenFit: 'screen' | 'stretch' | 'original';
  /** Mbit/s, custom quality only. */
  bitrate: number;
  /** Custom quality only, HEVC/AV1. */
  tenBit: boolean;
  /** AAC kbps for the game and mix tracks. */
  audioBitrate: number;
  clipsDir: string;
  /** A language code from locales/index.ts, null = follow Windows. */
  language: string | null;
  accent: string;
  sidebarCollapsed: boolean;
  launchAtStartup: boolean;
  closeToTray: boolean;
  microphone: boolean;
  /** WASAPI endpoint id, null = Windows default. */
  microphoneDevice: string | null;
  captureSound: boolean;
  overlay: boolean;
  /** Oldest non-favorite clips go to the Recycle Bin beyond this. null = no limit. */
  storageLimitGb: number | null;
  /** First-run wizard done. */
  onboarded: boolean;
}

/** What the engine would record with given settings. */
export interface ResolvedVideo {
  backend: Backend;
  codec: Codec;
  tenBit: boolean;
  width: number;
  height: number;
  fps: number;
  /** Average bit/s to expect. */
  bitrate: number;
}

export interface EncoderInfo {
  gpu: string;
  vendor: 'nvidia' | 'amd' | 'intel' | null;
  nvenc: boolean;
  codecs: Record<Codec, boolean>;
  tenBit: { hevc: boolean; av1: boolean };
  /** Whether Windows can play these formats on this PC. */
  decoders: { hevc: boolean; av1: boolean };
  refreshRate: number;
  width: number;
  height: number;
}

export interface Microphone {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface StorageInfo {
  usedBytes: number;
  clipCount: number;
  freeBytes: number;
}

/** A part of a screenshot, in its pixels. */
export interface Crop {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A PNG in <clipsDir>/Screenshots/<Game>/. */
export interface Screenshot {
  /** Path relative to the clips folder, forward slashes. */
  id: string;
  path: string;
  game: string | null;
  createdAt: number;
  size: number;
  width: number;
  height: number;
}

export interface Shard {
  /** Path relative to the clips folder, forward slashes. */
  id: string;
  path: string;
  /** null when the file still has its automatic name. */
  title: string | null;
  /** Game subfolder, null for clips at the root (desktop). */
  game: string | null;
  duration: number;
  size: number;
  createdAt: number;
  favorite: boolean;
}

export interface CaptureStatus {
  state: 'starting' | 'ready' | 'paused' | 'error';
  /** 'other': a hardware encoder from another maker. */
  encoder: 'nvidia' | 'amd' | 'intel' | 'software' | 'other' | null;
  width: number;
  height: number;
  fps: number;
  bufferedSeconds: number;
  memoryBytes: number;
  microphone: boolean;
  error: string | null;
  audioError: string | null;
  codec: Codec | null;
  tenBit: boolean;
  /** bit/s */
  bitrate: number;
  backend: Backend | null;
  /** Continuous recording in progress (the replay keeps working meanwhile). */
  recording: { seconds: number; bytes: number } | null;
}

/** What the editor needs to know about a clip. */
export interface ShardInfo {
  duration: number;
  width: number;
  height: number;
  fps: number;
  audioTracks: { name: string; kind: 'mix' | 'game' | 'mic' | 'other' }[];
}

/** Peak level (0..1 of full scale) per time slice, for one audio track. */
export interface Waveform {
  name: string;
  kind: ShardInfo['audioTracks'][number]['kind'];
  peaks: number[];
}

export interface ExportRequest {
  id: string;
  /** Seconds, within the clip. */
  start: number;
  end: number;
  /** 0 to 2, 1 = unchanged. `mic` only matters when the clip has a separate microphone track. */
  gains: { game: number; mic: number };
  /** copy: a new clip next to the original. discord: a file under `maxBytes`, put on the clipboard.
   *  file: written to `path` (chosen with chooseExportPath), all tracks, original quality. */
  target: 'copy' | 'discord' | 'file';
  maxBytes: number | null;
  path?: string | null;
  /** Burn a small "by krak" badge in the top right corner (forces a re-encode). */
  watermark?: boolean;
  /** Stretch a clip narrower than 16:9 (a stretched 4:3) over 16:9 (forces a re-encode). */
  stretch?: boolean;
}

export interface ExportResult {
  path: string;
  size: number;
  shard: Shard | null;
  reencoded: boolean;
}

export const api = {
  getSettings: () => invoke<Settings>('get_settings'),
  updateSettings: (patch: Partial<Settings>) => invoke<Settings>('update_settings', { patch }),
  chooseClipsDir: () => invoke<Settings | null>('choose_clips_dir'),
  listShards: () => invoke<Shard[]>('list_shards'),
  renameShard: (id: string, title: string) => invoke<Shard>('rename_shard', { id, title }),
  setFavorite: (id: string, favorite: boolean) => invoke<Shard>('set_favorite', { id, favorite }),
  deleteShards: (ids: string[]) => invoke<void>('delete_shards', { ids }),
  revealShard: (id: string) => invoke<void>('reveal_shard', { id }),
  copyShard: (id: string) => invoke<void>('copy_shard', { id }),
  openClipsDir: () => invoke<void>('open_clips_dir'),
  getCaptureStatus: () => invoke<CaptureStatus>('get_capture_status'),
  setCapturePaused: (paused: boolean) => invoke<CaptureStatus>('set_capture_paused', { paused }),
  saveReplay: () => invoke<void>('save_replay'),
  probeShard: (id: string) => invoke<ShardInfo>('probe_shard', { id }),
  exportShard: (request: ExportRequest) => invoke<ExportResult>('export_shard', { request }),
  cancelExport: () => invoke<void>('cancel_export'),
  getEncoderInfo: () => invoke<EncoderInfo>('get_encoder_info'),
  resolveVideo: (patch: Partial<Settings>) => invoke<ResolvedVideo>('resolve_video', { patch }),
  listMicrophones: () => invoke<Microphone[]>('list_microphones'),
  getStorageInfo: () => invoke<StorageInfo>('get_storage_info'),
  startRecording: () => invoke<CaptureStatus>('start_recording'),
  /** The finished file arrives as a `recording-saved` event. */
  stopRecording: () => invoke<CaptureStatus>('stop_recording'),
  takeScreenshot: () => invoke<void>('take_screenshot'),
  listScreenshots: () => invoke<Screenshot[]>('list_screenshots'),
  deleteScreenshots: (ids: string[]) => invoke<void>('delete_screenshots', { ids }),
  /** The whole image, or only `crop` (in the screenshot's pixels). */
  copyScreenshot: (id: string, crop?: Crop) => invoke<void>('copy_screenshot', { id, crop }),
  saveScreenshotCrop: (id: string, crop: Crop) => invoke<Screenshot>('save_screenshot_crop', { id, crop }),
  revealScreenshot: (id: string) => invoke<void>('reveal_screenshot', { id }),
  getPresence: () => invoke<{ enabled: boolean; connected: boolean }>('get_presence'),
  getWaveforms: (id: string, buckets: number) => invoke<Waveform[]>('get_waveforms', { id, buckets }),
  /** Native "Save as" dialog; null when cancelled. */
  chooseExportPath: (suggestedName: string) => invoke<string | null>('choose_export_path', { suggestedName }),
  /** Shows a file made by an export (this session) in Explorer. */
  revealExport: (path: string) => invoke<void>('reveal_export', { path }),
};

type Events = {
  'library-changed': null;
  'capture-status': CaptureStatus;
  'shard-saved': Shard;
  'recording-saved': Shard;
  'screenshot-saved': Screenshot;
  'recording-failed': string;
  'capture-failed': string;
  'export-progress': { id: string; progress: number };
  navigate: string;
};

export function on<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void): Promise<UnlistenFn> {
  return listen<Events[E]>(event, (e) => handler(e.payload));
}

export const mediaSrc = (path: string) => convertFileSrc(path);

/** Backend errors are short codes ("exists", "hotkey_taken"…); anything else is unexpected. */
export function errorCode(e: unknown): string {
  return typeof e === 'string' ? e : 'unknown';
}
