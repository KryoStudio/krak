// Typed bridge to the Rust backend. Outside Tauri (plain browser), `native` is false
// and the stores fall back to demo data.
import { convertFileSrc, invoke, isTauri } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export const native = isTauri();

export type Quality = 'balanced' | 'high' | 'ultra';

export interface Settings {
  hotkey: string;
  bufferSeconds: number;
  fps: number;
  quality: Quality;
  clipsDir: string;
  language: 'fr' | 'en' | null;
  accent: string;
  sidebarCollapsed: boolean;
  launchAtStartup: boolean;
  closeToTray: boolean;
  microphone: boolean;
  captureSound: boolean;
  overlay: boolean;
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
  encoder: 'nvidia' | 'amd' | 'intel' | 'software' | null;
  width: number;
  height: number;
  fps: number;
  bufferedSeconds: number;
  memoryBytes: number;
  microphone: boolean;
  error: string | null;
  audioError: string | null;
}

/** What the editor needs to know about a clip. */
export interface ShardInfo {
  duration: number;
  width: number;
  height: number;
  fps: number;
  audioTracks: { name: string; kind: 'mix' | 'game' | 'mic' | 'other' }[];
}

export interface ExportRequest {
  id: string;
  /** Seconds, within the clip. */
  start: number;
  end: number;
  /** 0 to 2, 1 = unchanged. `mic` only matters when the clip has a separate microphone track. */
  gains: { game: number; mic: number };
  /** copy: a new clip next to the original. discord: a file under `maxBytes`, put on the clipboard. */
  target: 'copy' | 'discord';
  maxBytes: number | null;
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
};

type Events = {
  'library-changed': null;
  'capture-status': CaptureStatus;
  'shard-saved': Shard;
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
