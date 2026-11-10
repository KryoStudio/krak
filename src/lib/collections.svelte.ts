// Collections: named groups of shards the user makes in the Glacier ("Clutches", "With the
// squad"…), like playlists: the files stay where they are. Kept by the backend in library.json,
// which also follows renames and deletes; in the browser preview, in local storage.
import { api, native, type Collection, type Shard } from './api';
import { t } from './i18n.svelte';
import { openMenu, toast, ui } from './ui.svelte';

const DEMO_KEY = 'krak.collections';

class Collections {
  list = $state<Collection[]>([]);

  async load() {
    try {
      if (native) this.list = await api.getCollections();
      else this.list = JSON.parse(localStorage.getItem(DEMO_KEY) ?? '[]');
    } catch {
      this.list = [];
    }
  }

  find(id: string): Collection | undefined {
    return this.list.find((c) => c.id === id);
  }

  /** The collections a shard is in. */
  of(shardId: string): Collection[] {
    return this.list.filter((c) => c.shards.includes(shardId));
  }

  /** A new collection (named "Collection n" when no name is given), with these shards. */
  create(name?: string, shards: string[] = []): Collection {
    const used = new Set(this.list.map((c) => c.name));
    let n = this.list.length + 1;
    while (!name && used.has(t('collections.defaultName', { n }))) n++;
    const collection: Collection = {
      id: `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
      name: name?.trim() || t('collections.defaultName', { n }),
      shards: [...new Set(shards)],
    };
    this.list.push(collection);
    void this.#save();
    return collection;
  }

  rename(id: string, name: string) {
    const collection = this.find(id);
    if (!collection || !name.trim() || name.trim() === collection.name) return;
    collection.name = name.trim();
    void this.#save();
  }

  delete(id: string) {
    this.list = this.list.filter((c) => c.id !== id);
    if (ui.filter === `collection:${id}`) ui.filter = 'all';
    void this.#save();
  }

  add(id: string, shardIds: string[]) {
    const collection = this.find(id);
    if (!collection) return;
    const added = shardIds.filter((s) => !collection.shards.includes(s));
    if (!added.length) return;
    collection.shards.push(...added);
    void this.#save();
    toast({ kind: 'info', title: t('collections.added', { name: collection.name, count: added.length }) });
  }

  remove(id: string, shardIds: string[]) {
    const collection = this.find(id);
    if (!collection) return;
    collection.shards = collection.shards.filter((s) => !shardIds.includes(s));
    void this.#save();
  }

  /** A shard got a new id (renamed): the backend already moved it, follow along. */
  moved(from: string, to: string) {
    for (const c of this.list) c.shards = c.shards.map((s) => (s === from ? to : s));
    if (!native) void this.#save();
  }

  /** Shards deleted: the backend already forgot them, follow along. */
  dropped(ids: string[]) {
    for (const c of this.list) c.shards = c.shards.filter((s) => !ids.includes(s));
    if (!native) void this.#save();
  }

  /** The menu to put `shards` in a collection (or a new one), at (x, y). */
  menu(x: number, y: number, shards: Shard[]) {
    const ids = shards.map((s) => s.id);
    const all = (c: Collection) => ids.every((id) => c.shards.includes(id));
    // After the menu that opened this one has closed.
    setTimeout(() =>
      openMenu(x, y, [
        ...this.list.map((c) => ({
          label: c.name,
          icon: all(c) ? ('check' as const) : ('folder' as const),
          run: () => (all(c) ? this.remove(c.id, ids) : this.add(c.id, ids)),
        })),
        ...(this.list.length ? (['separator'] as const) : []),
        {
          label: t('collections.new'),
          icon: 'plus' as const,
          run: () => {
            const created = this.create(undefined, ids);
            toast({ kind: 'info', title: t('collections.added', { name: created.name, count: ids.length }) });
          },
        },
      ]),
    );
  }

  async #save() {
    try {
      if (native) await api.saveCollections($state.snapshot(this.list));
      else localStorage.setItem(DEMO_KEY, JSON.stringify(this.list));
    } catch {
      toast({ kind: 'error', title: t('collections.saveFailed') });
    }
  }
}

export const collections = new Collections();
