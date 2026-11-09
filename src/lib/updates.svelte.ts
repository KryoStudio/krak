// In-app updates: signed builds published on GitHub Releases (see .github/workflows/release.yml).
//
// At startup (and every few hours for a Krak that lives in the tray), an update is offered in
// the main window; started in the tray, the tray icon gets a badge and the offer waits for the
// window. "Later" puts that version off for a day.
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { api, native } from './api';
import { t } from './i18n.svelte';
import { toast } from './ui.svelte';

const LATER_KEY = 'krak.update-later';
const LATER_MS = 24 * 60 * 60 * 1000;

/** Whether `version` was put off less than a day ago (`now` in ms). */
export function postponed(saved: string | null, version: string, now: number): boolean {
  if (!saved) return false;
  try {
    const later = JSON.parse(saved) as { version?: unknown; until?: unknown };
    return later.version === version && typeof later.until === 'number' && now < later.until;
  } catch {
    return false;
  }
}

/** A line or two from the release notes for the offer, without what only concerns a download
 *  from the release page (the installer's name, its checksum, the virus scan). */
export function notesSummary(notes: string | null | undefined, max = 220): string {
  if (!notes) return '';
  const download = /setup\.exe|sha-?256|virustotal|checksum|download/i;
  const paragraph = notes
    .replace(/\r/g, '')
    .split(/\n\s*\n/)
    .map((p) =>
      p
        .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1') // links and images: their text
        .replace(/^\s{0,3}(#{1,6}\s+|[-*+]\s+|>\s?)/gm, '') // headings, bullets, quotes
        .replace(/[*_`~]/g, '')
        .replace(/\s+/g, ' ')
        .trim(),
    )
    .find((p) => p && !download.test(p));
  if (!paragraph) return '';
  return paragraph.length > max ? `${paragraph.slice(0, max - 1).trimEnd()}…` : paragraph;
}

class Updates {
  available = $state<Update | null>(null);
  checking = $state(false);
  /** Download progress 0..1 while installing, null otherwise. */
  installing = $state<number | null>(null);
  /** The offer is up in the main window. */
  offered = $state(false);

  /** What's new, from the release notes ('' when they only speak of the download). */
  readonly summary = $derived(notesSummary(this.available?.body));

  /** Startup and background checks (`silent`) offer what they find, unless it was put off;
   *  a check from the settings answers either way. */
  async check(silent: boolean) {
    if (!native || this.checking || this.installing !== null) return;
    this.checking = true;
    try {
      this.available = await check();
      const version = this.available?.version;
      if (version && silent) this.offer(version);
      else if (version) {
        toast({
          kind: 'info',
          title: t('update.available', { version }),
          action: { label: t('update.install'), run: () => this.install() },
        });
      } else {
        this.offered = false;
        badge(null);
        if (!silent) toast({ kind: 'success', title: t('update.upToDate') });
      }
    } catch (e) {
      console.warn('update check failed', e);
      if (!silent) toast({ kind: 'error', title: t('update.failed') });
    } finally {
      this.checking = false;
    }
  }

  private offer(version: string) {
    if (postponed(read(LATER_KEY), version, Date.now())) {
      badge(null);
      return;
    }
    this.offered = true;
    badge(version);
  }

  /** Puts the offered version off for a day. */
  later() {
    const version = this.available?.version;
    this.offered = false;
    badge(null);
    if (version) write(LATER_KEY, JSON.stringify({ version, until: Date.now() + LATER_MS }));
  }

  async install() {
    const update = this.available;
    if (!update || this.installing !== null) return;
    this.installing = 0;
    let total = 0;
    let done = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') total = event.data.contentLength ?? 0;
        else if (event.event === 'Progress') {
          done += event.data.chunkLength;
          if (total) this.installing = done / total;
        }
      });
      // The Windows installer usually closes the app itself; if not, restart into the new version.
      await relaunch();
    } catch (e) {
      console.warn('update install failed', e);
      toast({ kind: 'error', title: t('update.failed') });
      this.installing = null;
    }
  }
}

/** The tray's badge, for a Krak started in the tray (and in case it goes back there). */
function badge(version: string | null) {
  if (native) api.setUpdateBadge(version).catch(() => {});
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // no storage: the offer comes back at the next start
  }
}

export const updates = new Updates();
