// In-app updates: signed builds published on GitHub Releases (see .github/workflows/release.yml).
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { native } from './api';
import { t } from './i18n.svelte';
import { toast } from './ui.svelte';

class Updates {
  available = $state<Update | null>(null);
  checking = $state(false);
  /** Download progress 0..1 while installing, null otherwise. */
  installing = $state<number | null>(null);

  /** Silent checks (at startup) only speak up when there is something to install. */
  async check(silent: boolean) {
    if (!native || this.checking) return;
    this.checking = true;
    try {
      this.available = await check();
      if (this.available) {
        const version = this.available.version;
        toast({
          kind: 'info',
          title: t('update.available', { version }),
          action: { label: t('update.install'), run: () => this.install() },
        });
      } else if (!silent) toast({ kind: 'success', title: t('update.upToDate') });
    } catch (e) {
      console.warn('update check failed', e);
      if (!silent) toast({ kind: 'error', title: t('update.failed') });
    } finally {
      this.checking = false;
    }
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

export const updates = new Updates();
