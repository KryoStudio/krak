import type { IconName } from './components/Icon.svelte';

/** 'desktop' = clips outside any game folder; `game:<name>` = one game. */
export type Filter = 'all' | 'favorites' | 'desktop' | `game:${string}`;
export type Sort = 'recent' | 'oldest' | 'longest' | 'largest' | 'name';

export const ui = $state({
  filter: 'all' as Filter,
  query: '',
  sort: 'recent' as Sort,
  /** Shard id open in the player. */
  playing: null as string | null,
  /** Shard id whose title is being edited. */
  renaming: null as string | null,
  /** The player shows the trim / audio / export tools. */
  editing: false,
  /** Shard just captured: its card is highlighted for a moment. */
  fresh: null as string | null,
});

export const filterFor = (game: string | null): Filter => (game === null ? 'desktop' : `game:${game}`);

// Toasts -------------------------------------------------------------------------

export interface Toast {
  id: number;
  kind: 'capture' | 'success' | 'error' | 'info';
  title: string;
  body?: string;
  action?: { label: string; run: () => void };
}

let nextToast = 0;
export const toasts = $state<Toast[]>([]);

export function toast(t: Omit<Toast, 'id'>) {
  toasts.push({ ...t, id: nextToast++ });
  if (toasts.length > 4) toasts.shift();
}

export function dismissToast(id: number) {
  const i = toasts.findIndex((t) => t.id === id);
  if (i >= 0) toasts.splice(i, 1);
}

// Context menu --------------------------------------------------------------------

export type MenuItem =
  | { label: string; icon?: IconName; shortcut?: string; danger?: boolean; run: () => void }
  | 'separator';

export const menu = $state({ open: false, x: 0, y: 0, items: [] as MenuItem[] });

export function openMenu(x: number, y: number, items: MenuItem[]) {
  Object.assign(menu, { open: true, x, y, items });
}

// Confirm dialog ------------------------------------------------------------------

interface ConfirmOptions {
  title: string;
  body: string;
  confirm: string;
  danger?: boolean;
}

export const dialog = $state({ open: false, options: null as ConfirmOptions | null, resolve: (_: boolean) => {} });

export function confirm(options: ConfirmOptions): Promise<boolean> {
  dialog.resolve(false);
  return new Promise((resolve) => Object.assign(dialog, { open: true, options, resolve }));
}
