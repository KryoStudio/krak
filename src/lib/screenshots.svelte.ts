// Screenshots: PNGs saved by the screenshot hotkey in <clipsDir>/Screenshots/<Game>/.
import { api, errorCode, native, type Crop, type Screenshot } from './api';
import { t } from './i18n.svelte';
import { library } from './library.svelte';
import { showError } from './shard-actions';
import { toast } from './ui.svelte';

function demoScreens(): Screenshot[] {
  const games = ['Fortnite', 'Rocket League', 'GTA V', 'Valorant', null];
  return Array.from({ length: 9 }, (_, i) => {
    const game = games[i % games.length];
    return {
      id: `Screenshots/${game ?? 'Desktop'}/Demo ${i}.png`,
      path: '',
      game,
      createdAt: Date.now() - (i * 7 + 3) * 60_000 * (i + 1),
      size: 2_400_000 + i * 180_000,
      width: 1920,
      height: 1080,
    };
  });
}

class Screens {
  items = $state<Screenshot[]>([]);
  loading = $state(true);
  #demo = $derived(!native || library.demo);

  games = $derived([...new Set(this.items.map((s) => s.game))].sort((a, b) => (a === null ? 1 : b === null ? -1 : a.localeCompare(b))));

  async load() {
    try {
      this.items = this.#demo ? demoScreens() : await api.listScreenshots().catch(() => []);
    } finally {
      this.loading = false;
    }
  }

  find(id: string) {
    return this.items.find((s) => s.id === id);
  }

  upsert(shot: Screenshot) {
    const i = this.items.findIndex((s) => s.id === shot.id);
    if (i >= 0) this.items[i] = shot;
    else this.items.unshift(shot);
  }

  async remove(ids: string[]) {
    if (!this.#demo) await api.deleteScreenshots(ids).catch((e) => Promise.reject(errorCode(e)));
    this.items = this.items.filter((s) => !ids.includes(s.id));
  }

  /** The image on the clipboard, or only `crop` of it. */
  async copy(shot: Screenshot, crop?: Crop) {
    if (this.#demo) {
      toast({ kind: 'info', title: t('toast.demo') });
      return;
    }
    try {
      await api.copyScreenshot(shot.id, crop && pixels(crop));
      toast({ kind: 'success', title: t('screens.copied'), body: t('toast.copiedBody') });
    } catch (e) {
      showError(errorCode(e));
    }
  }

  /** Saves `crop` of the screenshot as a new one next to it; returns it (undefined on failure). */
  async saveCrop(shot: Screenshot, crop: Crop): Promise<Screenshot | undefined> {
    if (this.#demo) {
      toast({ kind: 'info', title: t('toast.demo') });
      return;
    }
    try {
      const saved = await api.saveScreenshotCrop(shot.id, pixels(crop));
      this.upsert(saved);
      toast({ kind: 'success', title: t('screens.cropSaved') });
      return saved;
    } catch (e) {
      showError(errorCode(e));
    }
  }

  reveal(shot: Screenshot) {
    if (this.#demo) toast({ kind: 'info', title: t('toast.demo') });
    else api.revealScreenshot(shot.id).catch((e) => showError(errorCode(e)));
  }
}

export const screens = new Screens();

/** A crop on whole pixels (the cropper works in fractions of them). */
const pixels = (c: Crop): Crop => {
  const x = Math.round(c.x);
  const y = Math.round(c.y);
  return { x, y, width: Math.round(c.x + c.width) - x, height: Math.round(c.y + c.height) - y };
};

/** Same as the screenshot hotkey; the file arrives as a `screenshot-saved` event. */
export async function takeScreenshot() {
  if (!native) {
    const demo = demoScreens()[0];
    screens.upsert({ ...demo, id: `Screenshots/Preview ${Date.now()}.png`, createdAt: Date.now() });
    toast({ kind: 'success', title: t('screens.saved') });
    return;
  }
  try {
    await api.takeScreenshot();
  } catch (e) {
    const code = errorCode(e);
    if (code.includes('not found')) toast({ kind: 'info', title: t('screens.soon') });
    else showError(code);
  }
}
