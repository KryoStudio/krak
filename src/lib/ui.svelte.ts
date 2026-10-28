import type { GameId } from './demo';

export type Filter = 'all' | 'favorites' | GameId;

export const ui = $state({
  filter: 'all' as Filter,
  query: '',
  bufferSeconds: 30,
  hotkey: 'F9',
});
