// The best-of being put together: shards one after another, each as edited in the editor (its
// trim, volumes, cuts, slow motion, mutes, blurs, captions: one place to edit a clip), with
// transitions, an intro title, music, in 16:9 or vertical 9:16. Kept per PC (local storage) so
// it survives a restart; rendered by the backend's composition engine.
import type { Composition, Shard } from './api';
import { getEdit } from './edits';
import { emptyEffects, playedLength, type Effects } from './fx.svelte';
import { settings } from './settings.svelte';

export type Transition = 'cut' | 'fade' | 'flash';

export interface Music {
  path: string;
  /** File name, shown. */
  name: string;
  /** 0..1. */
  volume: number;
  /** Seconds into the song where it starts. */
  offset: number;
  /** Lowered under loud moments of the clips. */
  duck: boolean;
}

/** The intro title card lasts this long (the backend's `TITLE_SECONDS`). */
export const TITLE_SECONDS = 2.5;
const KEY = 'krak.bestof';
/** Quick fills take at most this many shards. */
const QUICK_FILL = 15;

interface Saved {
  items: string[];
  transition: Transition;
  titleOn: boolean;
  title: string;
  music: Music | null;
  vertical: boolean;
}

function read(): Partial<Saved> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? '{}');
  } catch {
    return {};
  }
}

const saved = read();

export const bestof = $state({
  /** Shard ids, in order. */
  items: Array.isArray(saved.items) ? saved.items.filter((id) => typeof id === 'string') : ([] as string[]),
  transition: (['cut', 'fade', 'flash'] as const).includes(saved.transition as Transition) ? (saved.transition as Transition) : 'fade',
  titleOn: saved.titleOn ?? true,
  title: typeof saved.title === 'string' ? saved.title : '',
  music: saved.music ?? (null as Music | null),
  vertical: !!saved.vertical,
});

/** Writes the best-of to local storage (called by the page on every change). */
export function persist() {
  try {
    const { items, transition, titleOn, title, music, vertical } = bestof;
    localStorage.setItem(KEY, JSON.stringify({ items, transition, titleOn, title, music, vertical }));
  } catch {
    // no storage: kept for this session only
  }
}

/** A clip as the best-of plays it: its saved edit, or the whole clip. */
export function clipOf(shard: Shard): { start: number; end: number; gains: { game: number; mic: number }; fx: Effects; cropX: number } {
  const edit = getEdit(shard.id);
  const duration = shard.duration || edit?.end || 30;
  return {
    start: edit ? Math.max(0, edit.start) : 0,
    end: edit ? Math.min(duration, edit.end) : duration,
    gains: edit?.gains ?? { game: 1, mic: 1 },
    fx: edit?.fx ?? emptyEffects(),
    cropX: edit?.cropX ?? 0.5,
  };
}

/** How long a clip plays in the best-of. */
export function clipLength(shard: Shard): number {
  const clip = clipOf(shard);
  return playedLength(clip.start, clip.end, clip.fx);
}

export function add(ids: string[]) {
  for (const id of ids) if (!bestof.items.includes(id)) bestof.items.push(id);
}

export function removeAt(index: number) {
  bestof.items.splice(index, 1);
}

export function move(from: number, to: number) {
  if (from === to || from < 0 || from >= bestof.items.length) return;
  const [id] = bestof.items.splice(from, 1);
  bestof.items.splice(Math.max(0, Math.min(bestof.items.length, to)), 0, id);
}

/** Shards for a quick fill: favorites, the last week, or one game; newest first. */
export function quickPick(shards: Shard[], kind: 'favorites' | 'week' | { game: string | null }): string[] {
  const week = Date.now() - 7 * 24 * 3600 * 1000;
  return shards
    .filter((s) =>
      kind === 'favorites' ? s.favorite : kind === 'week' ? s.createdAt >= week : s.game === kind.game,
    )
    .sort((a, b) => b.createdAt - a.createdAt)
    .slice(0, QUICK_FILL)
    .reverse()
    .map((s) => s.id);
}

/** What the backend renders. */
export function composition(shards: Shard[], defaultTitle: string): Composition {
  return {
    clips: shards.map((shard) => {
      const clip = clipOf(shard);
      return { id: shard.id, start: clip.start, end: clip.end, gains: clip.gains, ...clip.fx, cropX: clip.cropX };
    }),
    transition: bestof.transition,
    title: bestof.titleOn ? bestof.title.trim() || defaultTitle : null,
    music: bestof.music ? { path: bestof.music.path, volume: bestof.music.volume, offset: bestof.music.offset, duck: bestof.music.duck } : null,
    vertical: bestof.vertical,
    watermark: settings.value.watermark,
  };
}
