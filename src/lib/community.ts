// The Krak Discord, reached through krakclip.com so the invite can change without an update.
import { openUrl } from '@tauri-apps/plugin-opener';
import { native } from './api';

const SITE = 'https://krakclip.com';

/** The Krak Discord's invite; `src` tells the site which button was clicked (no other data). */
export function discordInvite(src: string): string {
  return `${SITE}/discord?src=${encodeURIComponent(src)}`;
}

/** Opens the #clips channel (Discord asks to join first when needed). */
export const CLIPS_CHANNEL = `${SITE}/discord/clips`;

/** Opens a link in the default browser (or Discord, for its links). */
export function openLink(url: string): void {
  if (native) openUrl(url).catch(() => {});
  else window.open(url, '_blank', 'noopener');
}
