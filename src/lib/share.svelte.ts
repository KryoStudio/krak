// "Share to the Krak community": which clip the share sheet is open for, and how to export it.
import type { Shard } from './api';
import { getEdit } from './edits';
import { hasEffects, type Effects } from './fx.svelte';

export interface ShareRequest {
  shard: Shard;
  /** Seconds within the clip. */
  start: number;
  end: number;
  gains: { game: number; mic: number };
  vertical: boolean;
  cropX: number;
  /** Cuts, mutes, slow motion, blurs, captions: exported through a composition. */
  effects?: Effects | null;
}

export const share = $state({ request: null as ShareRequest | null });

/** Opens the sheet for a shard as it was last edited (or whole). */
export function shareShard(shard: Shard) {
  const edit = getEdit(shard.id);
  share.request = {
    shard,
    start: edit?.start ?? 0,
    // Past the end is fine: the backend stops at the clip's end.
    end: edit?.end ?? (shard.duration > 0 ? shard.duration : 24 * 60 * 60),
    gains: edit ? { ...edit.gains } : { game: 1, mic: 1 },
    vertical: !!edit?.vertical,
    cropX: edit?.cropX ?? 0.5,
    effects: hasEffects(edit?.fx) ? edit?.fx : null,
  };
}

const CONSENT_KEY = 'krak.community-consent';

/** The user has read what sharing sends, once. */
export function consented(): boolean {
  try {
    return localStorage.getItem(CONSENT_KEY) === '1';
  } catch {
    return false;
  }
}

export function giveConsent() {
  try {
    localStorage.setItem(CONSENT_KEY, '1');
  } catch {
    // no storage: asked again next time
  }
}

/** When a pairing code expires, in ms, from the server's ISO date or timestamp (seconds or
 *  milliseconds). */
export function expiryTime(expiresAt: unknown): number | null {
  if (typeof expiresAt === 'number' && Number.isFinite(expiresAt)) return expiresAt < 1e12 ? expiresAt * 1000 : expiresAt;
  if (typeof expiresAt === 'string') {
    const time = Date.parse(expiresAt);
    return Number.isNaN(time) ? null : time;
  }
  return null;
}
