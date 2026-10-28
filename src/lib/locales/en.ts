import type { fr } from './fr';

export const en: Record<keyof typeof fr, string> = {
  'capture.active': 'Capture on',
  'capture.hint': 'The last {seconds} seconds are kept.',
  'capture.key': 'to capture',
  'nav.glacier': 'Glacier',
  'nav.favorites': 'Favorites',
  'nav.games': 'Games',
  'nav.settings': 'Settings',
  'glacier.title': 'Glacier',
  'glacier.count': '{count} shards · {size}',
  'glacier.search': 'Search shards…',
  'glacier.all': 'All',
  'glacier.preview': 'Preview · demo data',
  'glacier.empty': 'No shards here yet.',
  'toast.title': 'Shard captured',
  'toast.open': 'Open',
  'game.desktop': 'Desktop',
  'about.by': 'by Kryo Studio',
};
