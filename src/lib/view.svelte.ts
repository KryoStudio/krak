// The shards on screen: the library through the current filter, search and sort.
import { t } from './i18n.svelte';
import { library } from './library.svelte';
import { displayTitle, gameName } from './shard-actions';
import { ui } from './ui.svelte';

/** Lowercase, without accents: "Évasion" matches "evasion". */
const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

class View {
  shards = $derived.by(() => {
    const q = fold(ui.query.trim());
    const f = ui.filter;
    const list = library.shards.filter((s) => {
      if (f === 'favorites' && !s.favorite) return false;
      if (f === 'desktop' && s.game !== null) return false;
      if (f.startsWith('game:') && s.game !== f.slice(5)) return false;
      return !q || fold(displayTitle(s)).includes(q) || fold(gameName(s)).includes(q);
    });
    const by = {
      recent: (a, b) => b.createdAt - a.createdAt,
      oldest: (a, b) => a.createdAt - b.createdAt,
      longest: (a, b) => b.duration - a.duration,
      largest: (a, b) => b.size - a.size,
      name: (a, b) => displayTitle(a).localeCompare(displayTitle(b)),
    } satisfies Record<typeof ui.sort, (a: (typeof list)[number], b: (typeof list)[number]) => number>;
    return list.sort(by[ui.sort]);
  });

  size = $derived(this.shards.reduce((sum, s) => sum + s.size, 0));

  /** The game shown on its own, if any (undefined otherwise; null = desktop). */
  game = $derived(ui.filter === 'desktop' ? null : ui.filter.startsWith('game:') ? ui.filter.slice(5) : undefined);

  title = $derived(
    ui.filter === 'all' ? t('nav.glacier') : ui.filter === 'favorites' ? t('nav.favorites') : (this.game ?? t('game.desktop')),
  );
}

export const view = new View();
