// What you can do with a shard, shared by the cards, the context menu and the player.
import { api, errorCode, type Shard } from './api';
import { hasKey, t } from './i18n.svelte';
import { library } from './library.svelte';
import { confirm, toast, ui, type MenuItem } from './ui.svelte';

export const gameName = (s: Shard) => s.game ?? t('game.desktop');
export const displayTitle = (s: Shard) => s.title ?? gameName(s);

/** Toasts the translated message of a backend error code. */
export function showError(code: unknown) {
  const key = `error.${code}`;
  toast({ kind: 'error', title: t(hasKey(key) ? key : 'error.unknown') });
}

function demoOnly(): boolean {
  if (library.demo) toast({ kind: 'info', title: t('toast.demo') });
  return library.demo;
}

/** A shard was just captured: show it in the Glacier right away, highlighted, and say so. */
export function announce(shard: Shard) {
  library.upsert(shard);
  ui.fresh = shard.id;
  setTimeout(() => ui.fresh === shard.id && (ui.fresh = null), 2600);
  toast({
    kind: 'capture',
    title: t('toast.captured'),
    body: `${gameName(shard)} · ${Math.round(shard.duration)} s`,
    action: { label: t('toast.view'), run: () => play(shard) },
  });
}

export const play = (s: Shard) => (ui.playing = s.id);
/** Opens the shard straight in the editor (trim, audio, export). */
export function edit(s: Shard) {
  ui.playing = s.id;
  ui.editing = true;
}
export const rename = (s: Shard) => (ui.renaming = s.id);

export function toggleFavorite(s: Shard) {
  library.toggleFavorite(s.id).catch(showError);
}

export async function copy(s: Shard) {
  if (demoOnly()) return;
  try {
    await api.copyShard(s.id);
    toast({ kind: 'success', title: t('toast.copied'), body: t('toast.copiedBody') });
  } catch (e) {
    showError(errorCode(e));
  }
}

export function reveal(s: Shard) {
  if (demoOnly()) return;
  api.revealShard(s.id).catch((e) => showError(errorCode(e)));
}

export async function remove(s: Shard) {
  const ok = await confirm({
    title: t('delete.title'),
    body: t('delete.body', { name: displayTitle(s) }),
    confirm: t('shard.delete'),
    danger: true,
  });
  if (!ok) return;
  try {
    await library.remove([s.id]);
    if (ui.playing === s.id) ui.playing = null;
    toast({ kind: 'info', title: t('toast.deleted') });
  } catch (e) {
    showError(e);
  }
}

export function shardMenu(s: Shard): MenuItem[] {
  return [
    { label: t('shard.play'), icon: 'play', shortcut: t('key.enter'), run: () => play(s) },
    { label: t('editor.edit'), icon: 'scissors', shortcut: 'E', run: () => edit(s) },
    { label: t('shard.rename'), icon: 'pencil', shortcut: 'F2', run: () => rename(s) },
    { label: t(s.favorite ? 'shard.unfavorite' : 'shard.favorite'), icon: 'star', shortcut: 'F', run: () => toggleFavorite(s) },
    'separator',
    { label: t('shard.copy'), icon: 'copy', shortcut: 'Ctrl+C', run: () => copy(s) },
    { label: t('shard.reveal'), icon: 'folder', run: () => reveal(s) },
    'separator',
    { label: t('shard.delete'), icon: 'trash', shortcut: t('key.delete'), danger: true, run: () => remove(s) },
  ];
}

/** Keyboard shortcuts on a focused card or in the player. Returns true when handled. */
export function handleShardKey(e: KeyboardEvent, s: Shard): boolean {
  if (e.key === 'F2') rename(s);
  else if (e.key === 'Delete') remove(s);
  else if (e.key.toLowerCase() === 'f' && !e.ctrlKey && !e.altKey) toggleFavorite(s);
  else if (e.key.toLowerCase() === 'e' && !e.ctrlKey && !e.altKey && !ui.editing) edit(s);
  else if (e.key.toLowerCase() === 'c' && e.ctrlKey) copy(s);
  else return false;
  e.preventDefault();
  return true;
}
