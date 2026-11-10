// What the editor does to the clip open in it beyond the trim and volumes: passages cut out,
// slow motion, muted or bleeped ranges, blurred zones and captions. The editor loads and saves
// them with the trim (`edits.ts`), the timeline and the overlay on the player change them, the
// player previews them (`at`), and an export with any of them goes through the backend's
// composition engine (`editor::compose`), which reads exactly these shapes.

export interface Range {
  start: number;
  end: number;
}

export interface Slow extends Range {
  /** 0.5 = half speed (0.25 at the slowest). */
  rate: number;
}

export type MuteTrack = 'game' | 'mic' | 'all';

export interface Mute extends Range {
  track: MuteTrack;
  /** A tone instead of silence. */
  beep: boolean;
}

/** A pixelated rectangle, as shares of the clip's picture; over its range, or the whole clip. */
export interface Blur {
  x: number;
  y: number;
  w: number;
  h: number;
  start?: number;
  end?: number;
}

export type CaptionStyle = 'shadow' | 'outline' | 'box';

/** Text centred on (x, y), shares of the exported frame; `size` a share of its height. */
export interface Caption extends Range {
  text: string;
  x: number;
  y: number;
  size: number;
  style: CaptionStyle;
}

export interface Effects {
  cuts: Range[];
  slow: Slow[];
  mutes: Mute[];
  blurs: Blur[];
  texts: Caption[];
}

export type Kind = 'cut' | 'slow' | 'mute' | 'blur' | 'text';

export const CAPTION_SIZES = { s: 0.05, m: 0.07, l: 0.1 } as const;

export function emptyEffects(): Effects {
  return { cuts: [], slow: [], mutes: [], blurs: [], texts: [] };
}

export const fx = $state({
  ...emptyEffects(),
  /** The item being edited (its handles show, the inspector edits it). */
  selected: null as { kind: Kind; index: number } | null,
  /** The editor's playhead (clip seconds), for what the overlay shows. */
  current: 0,
});

/** Starts editing `effects` (a saved edit's), or none. */
export function loadEffects(effects?: Partial<Effects> | null) {
  const clean = emptyEffects();
  for (const key of Object.keys(clean) as (keyof Effects)[]) {
    const list = effects?.[key];
    if (Array.isArray(list)) (clean[key] as unknown[]) = list.map((item) => ({ ...item }));
  }
  Object.assign(fx, clean, { selected: null });
}

/** A plain copy of the effects being edited. */
export function snapshot(): Effects {
  return {
    cuts: fx.cuts.map((c) => ({ ...c })),
    slow: fx.slow.map((s) => ({ ...s })),
    mutes: fx.mutes.map((m) => ({ ...m })),
    blurs: fx.blurs.map((b) => ({ ...b })),
    texts: fx.texts.map((c) => ({ ...c })),
  };
}

export function hasEffects(e: Effects | null | undefined): boolean {
  return !!e && (e.cuts.length > 0 || e.slow.length > 0 || e.mutes.length > 0 || e.blurs.length > 0 || e.texts.length > 0);
}

/** `start..end` without the cuts (what the backend's `timeline::kept` keeps). */
export function kept(start: number, end: number, cuts: Range[]): [number, number][] {
  const sorted = cuts.filter((c) => c.end > c.start).sort((a, b) => a.start - b.start);
  const parts: [number, number][] = [];
  let from = start;
  for (const cut of sorted) {
    if (cut.end <= from) continue;
    if (cut.start >= end) break;
    if (cut.start > from) parts.push([from, cut.start]);
    from = cut.end;
  }
  if (end > from) parts.push([from, end]);
  return parts;
}

/** The playback rate at clip time `t` (the slowest zone there, else 1). */
export function rateAt(e: Effects, t: number): number {
  let rate = 1;
  for (const zone of e.slow) if (t >= zone.start && t < zone.end) rate = Math.min(rate, Math.max(0.25, zone.rate));
  return rate;
}

/** How long `start..end` plays once cut and slowed. */
export function playedLength(start: number, end: number, e: Effects): number {
  let total = 0;
  for (const [from, to] of kept(start, end, e.cuts)) {
    const edges = [from, to];
    for (const zone of e.slow) for (const edge of [zone.start, zone.end]) if (edge > from && edge < to) edges.push(edge);
    edges.sort((a, b) => a - b);
    for (let i = 0; i + 1 < edges.length; i++) total += (edges[i + 1] - edges[i]) / rateAt(e, (edges[i] + edges[i + 1]) / 2);
  }
  return total;
}

/** The cut clip time `t` falls in, if any. */
export function cutAt(e: Effects, t: number): Range | null {
  return e.cuts.find((c) => t >= c.start && t < c.end) ?? null;
}

/** What the mutes leave of each track at clip time `t` (1 untouched, 0 muted), and how much
 *  bleep plays. */
export function muteAt(e: Effects, t: number): { game: number; mic: number; beep: number } {
  const out = { game: 1, mic: 1, beep: 0 };
  for (const m of e.mutes) {
    if (t < m.start || t >= m.end) continue;
    if (m.track !== 'mic') out.game = 0;
    if (m.track !== 'game') out.mic = 0;
    if (m.beep) out.beep = 1;
  }
  return out;
}

/** How visible an item shown over `start..end` is at `t` (fading over `fade` seconds). */
export function presence(start: number, end: number, t: number, fade = 0.2): number {
  if (t < start || t >= end) return 0;
  return Math.max(0, Math.min(1, (t - start) / fade, (end - t) / fade));
}

/** Removes the item `kind` #`index`, and the selection if it was that one. */
export function remove(kind: Kind, index: number) {
  const list = listOf(kind);
  list.splice(index, 1);
  if (fx.selected?.kind === kind) fx.selected = fx.selected.index === index ? null : fx.selected.index > index ? { kind, index: fx.selected.index - 1 } : fx.selected;
}

export function listOf(kind: Kind): unknown[] {
  return { cut: fx.cuts, slow: fx.slow, mute: fx.mutes, blur: fx.blurs, text: fx.texts }[kind];
}

/** Adds an item and selects it. */
export function add<K extends Kind>(kind: K, item: K extends 'cut' ? Range : K extends 'slow' ? Slow : K extends 'mute' ? Mute : K extends 'blur' ? Blur : Caption) {
  const list = listOf(kind);
  list.push(item);
  fx.selected = { kind, index: list.length - 1 };
}
