// Typed bridge to the Rust backend. Outside Tauri (plain browser), `native` is false
// and the stores fall back to demo data.
import { convertFileSrc, invoke, isTauri } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Effects } from './fx.svelte';

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
  /** Puts a marker, found again on the editor's timeline. */
  markerHotkey: string;
  /** Controller buttons held together to keep a shard ("LB+RB"), '' = none. */
  controllerHotkey: string;
  /** A short buzz when the controller shortcut keeps a shard, two if it failed. */
  controllerRumble: boolean;
  /** A press within this many seconds of the previous one extends that shard (0 = never). */
  extendSeconds: number;
  /** Show what you play and clip on your Discord profile. */
  discordPresence: boolean;
  /** Exports carry a small "by krak" badge (off by default: no watermark unless asked). */
  watermark: boolean;
  /** Clips play as last edited: the kept part, at the editor's volumes. */
  playEdited: boolean;
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
  /** The whole screen, or only the game's window ("stream-proof"). */
  captureSource: 'screen' | 'game';
  /** The app filmed in game-only capture; null = the game in front, found by Krak. */
  captureGame: AppPick | null;
  /** GDI name of the display to record; null = the main one. */
  monitor: string | null;
  /** Draw the mouse cursor in clips. */
  showCursor: boolean;
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
  /** The one-time invitation to the Krak Discord: not shown yet, put off, answered. */
  communityInvite: 'pending' | 'later' | 'joined' | 'never';
  /** When it was put off ("Not now"), in ms since 1970. */
  communityInviteAt: number | null;
}

/** A shard that recompression would make lighter. */
export interface RecompressCandidate {
  id: string;
  size: number;
  /** Its size once recompressed, roughly. */
  estimate: number;
}

export interface Recompressed {
  done: number;
  skipped: number;
  /** Bytes the originals take in the Recycle Bin. */
  saved: number;
}

/** A named group of shards (the files stay where they are). */
export interface Collection {
  id: string;
  name: string;
  /** Shard ids, in the order they were added. */
  shards: string[];
}

/** An app picked for game-only capture. */
export interface AppPick {
  /** Program file name without extension, lowercase. */
  exe: string;
  name: string;
}

/** An app with a window open, for the game-only capture picker. */
export interface OpenApp extends AppPick {
  /** Its main window's title. */
  title: string;
  /** Krak would pick it by itself. */
  game: boolean;
}

/** A display that can be recorded. */
export interface DisplayInfo {
  /** GDI name ("\.\DISPLAY1"), what the setting stores. */
  id: string;
  /** The monitor's own name, or empty. */
  name: string;
  width: number;
  height: number;
  primary: boolean;
}

/** One clip of a composition, with what the editor did to it (see `fx.svelte.ts`). */
export interface CompositionClip {
  id: string;
  start: number;
  end: number;
  gains: { game: number; mic: number };
  cuts?: Effects['cuts'];
  slow?: Effects['slow'];
  mutes?: Effects['mutes'];
  blurs?: Effects['blurs'];
  texts?: Effects['texts'];
  /** Vertical: where the 9:16 window is (0 left, 1 right). */
  cropX?: number;
}

/** Clips one after another (the backend's `editor::compose::Composition`). */
export interface Composition {
  clips: CompositionClip[];
  transition?: 'cut' | 'fade' | 'flash';
  title?: string | null;
  music?: { path: string; volume: number; offset: number; duck: boolean } | null;
  vertical?: boolean;
  watermark?: boolean;
}

export interface CompositionRequest extends Composition {
  target: 'copy' | 'file' | 'discord' | 'share' | 'glacier';
  maxBytes?: number | null;
  path?: string | null;
  /** Glacier: the new shard's name. */
  name?: string | null;
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
  /** Only the game's window is recorded. */
  gameOnly: boolean;
  /** In game-only capture, the game being filmed (null: waiting for one). */
  filming: string | null;
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
   *  file: written to `path` (chosen with chooseExportPath), all tracks, original quality.
   *  share: like discord, for the Krak community's bot (not put on the clipboard). */
  target: 'copy' | 'discord' | 'file' | 'share';
  maxBytes: number | null;
  path?: string | null;
  /** Burn a small "by krak" badge in the top right corner (forces a re-encode). */
  watermark?: boolean;
  /** Stretch a clip narrower than 16:9 (a stretched 4:3) over 16:9 (forces a re-encode). */
  stretch?: boolean;
  /** Vertical 9:16 (1080×1920), a window of the picture (forces a re-encode). */
  vertical?: boolean;
  /** Where that window is: 0 left, 1 right, 0.5 centred. */
  cropX?: number;
}

export interface ExportResult {
  path: string;
  size: number;
  shard: Shard | null;
  reencoded: boolean;
}

/** A press of the clip hotkey was accepted: the clip is being written (`shard-saved` follows). */
export interface SavingShard {
  game: string | null;
  /** Seconds. */
  duration: number;
  /** The previous shard grows (same file) instead of a new one. */
  extended: boolean;
}

/** The account signed in to the Discord app on this PC, read from Discord itself (no login). */
export interface DiscordUser {
  id: string;
  username: string;
  displayName: string | null;
  /** Avatar URL on Discord's CDN. */
  avatar: string;
}

export interface Presence {
  enabled: boolean;
  connected: boolean;
  /** Known while connected. */
  user: DiscordUser | null;
}

/** krakclip.com/api/community. `api` null: sharing from Krak is not open yet. */
export interface CommunityInfo {
  invite: string | null;
  guildId: string | null;
  clipsChannelId: string | null;
  api: string | null;
}

/** A Discord account, as the Krak bot describes it. */
export interface CommunityUser {
  id: string;
  username: string;
  globalName: string | null;
  avatarUrl: string | null;
}

export interface Pairing {
  code: string;
  /** dm: confirm in a DM from the bot. command: type `/share link code:<code>` in the server. */
  via: 'dm' | 'command';
  /** An ISO date or a timestamp. */
  expiresAt: unknown;
}

export type PairStatus =
  | { status: 'pending' }
  | { status: 'linked'; user: CommunityUser }
  | { status: 'expired' }
  | { status: 'denied' };

export interface CommunityMe {
  user: CommunityUser | null;
  canShare: boolean;
  reason: string | null;
  maxBytes: number;
  remaining: { hour: number | null; day: number | null };
  games: string[];
  clipsUrl: string | null;
  /** What the export must stay under; null when the server's limit is unusable. */
  shareLimit: number | null;
}

/** What community commands fail with. */
export interface CommunityError {
  code: string;
  reason: string | null;
  retryAfter: number | null;
  maxBytes: number | null;
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
  getPresence: () => invoke<Presence>('get_presence'),
  getWaveforms: (id: string, buckets: number) => invoke<Waveform[]>('get_waveforms', { id, buckets }),
  /** The game and microphone tracks decoded, for the live mix (layout in `live-mix.ts`). */
  getAudioStems: (id: string) => invoke<ArrayBuffer>('get_audio_stems', { id }),
  /** Waits (up to 10 s) for controller buttons held together then released: "LB+RB". */
  captureControllerCombo: () => invoke<string>('capture_controller_combo'),
  /** Shards older than `days` worth recompressing (never favorites). */
  recompressScan: (days: number) => invoke<RecompressCandidate[]>('recompress_scan', { days }),
  /** Recompresses them; progress as `export-progress` with the id "recompress". */
  recompressShards: (ids: string[]) => invoke<Recompressed>('recompress_shards', { ids }),
  getCollections: () => invoke<Collection[]>('get_collections'),
  saveCollections: (collections: Collection[]) => invoke<void>('save_collections', { collections }),
  /** The markers put while the clip was recorded, seconds into it. */
  getMarkers: (id: string) => invoke<number[]>('get_markers', { id }),
  /** Apps with a window open, games first (game-only capture picker). */
  listOpenApps: () => invoke<OpenApp[]>('list_open_apps'),
  listDisplays: () => invoke<DisplayInfo[]>('list_displays'),
  /** Renders clips with their edits (one clip's cuts and captions, or a best-of); progress
   *  comes as `export-progress` with the id "composition". */
  exportComposition: (request: CompositionRequest) => invoke<ExportResult>('export_composition', { request }),
  /** Native "Open" dialog for a song; null when cancelled. */
  chooseMusic: () => invoke<string | null>('choose_music'),
  /** Native "Save as" dialog; null when cancelled. */
  chooseExportPath: (suggestedName: string) => invoke<string | null>('choose_export_path', { suggestedName }),
  /** Shows a file made by an export (this session) in Explorer. */
  revealExport: (path: string) => invoke<void>('reveal_export', { path }),
  /** The tray icon's badge, menu entry and tooltip for an update that waits (null: none). */
  setUpdateBadge: (version: string | null) => invoke<void>('set_update_badge', { version }),
  /** The calls below reach the network: only once the user asked to share (or to unlink). */
  communityInfo: () => invoke<CommunityInfo>('community_info'),
  /** The linked account, from the encrypted token file (no network). */
  communityAccount: () => invoke<CommunityUser | null>('community_account'),
  communityPair: () => invoke<Pairing>('community_pair'),
  communityPairStatus: () => invoke<PairStatus>('community_pair_status'),
  communityMe: () => invoke<CommunityMe>('community_me'),
  /** Posts a file an export wrote this session; progress as `community-upload-progress`. */
  communityUpload: (path: string, title: string, game: string | null, spoiler: boolean) =>
    invoke<{ threadId: string; url: string }>('community_upload', { path, title, game, spoiler }),
  communityCancelUpload: () => invoke<void>('community_cancel_upload'),
  communityDelete: (threadId: string) => invoke<void>('community_delete', { threadId }),
  communityUnlink: () => invoke<void>('community_unlink'),
};

type Events = {
  'library-changed': null;
  'capture-status': CaptureStatus;
  'shard-saving': SavingShard;
  'marker-added': null;
  'shard-saved': Shard;
  'recording-saved': Shard;
  'screenshot-saved': Screenshot;
  'recording-failed': string;
  'capture-failed': string;
  'export-progress': { id: string; progress: number };
  navigate: string;
  /** "Update Krak" was picked in the tray menu. */
  'update-install': null;
  'community-upload-progress': { sent: number; total: number };
};

export function on<E extends keyof Events>(event: E, handler: (payload: Events[E]) => void): Promise<UnlistenFn> {
  return listen<Events[E]>(event, (e) => handler(e.payload));
}

export const mediaSrc = (path: string) => convertFileSrc(path);

/** Backend errors are short codes ("exists", "hotkey_taken"…); anything else is unexpected. */
export function errorCode(e: unknown): string {
  return typeof e === 'string' ? e : 'unknown';
}

/** A community command's error, or an unexpected one as such. */
export function communityError(e: unknown): CommunityError {
  if (e && typeof e === 'object' && typeof (e as CommunityError).code === 'string') return e as CommunityError;
  return { code: typeof e === 'string' ? e : 'community_failed', reason: null, retryAfter: null, maxBytes: null };
}
