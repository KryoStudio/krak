import { api, errorCode, native, type Shard } from './api';
import { demoShards } from './demo';

const DEMO_KEY = 'kryo-shard.demo';

function readDemoFlag(): boolean {
  try {
    return localStorage.getItem(DEMO_KEY) === '1';
  } catch {
    return false;
  }
}

class Library {
  shards = $state<Shard[]>([]);
  loading = $state(true);
  /** Demo data: always in a plain browser, opt-in (dev toggle) inside the app. */
  demo = $state(!native || readDemoFlag());

  games = $derived.by(() => {
    const counts = new Map<string | null, number>();
    for (const s of this.shards) counts.set(s.game, (counts.get(s.game) ?? 0) + 1);
    return [...counts]
      .map(([game, count]) => ({ game, count }))
      .sort((a, b) => (a.game === null ? 1 : b.game === null ? -1 : a.game.localeCompare(b.game)));
  });
  favorites = $derived(this.shards.filter((s) => s.favorite).length);

  // Renaming changes a shard's id (it is its path): keep a stable key per card so it isn't re-created.
  #keys = new Map<string, number>();
  #nextKey = 0;

  key(id: string): number {
    let k = this.#keys.get(id);
    if (k === undefined) this.#keys.set(id, (k = this.#nextKey++));
    return k;
  }

  find(id: string): Shard | undefined {
    return this.shards.find((s) => s.id === id);
  }

  async load() {
    try {
      this.shards = this.demo ? demoShards() : await api.listShards();
    } finally {
      this.loading = false;
    }
  }

  async setDemo(on: boolean) {
    this.demo = on;
    try {
      localStorage.setItem(DEMO_KEY, on ? '1' : '0');
    } catch {
      // no storage
    }
    await this.load();
  }

  /** Adds a shard (newest first), or refreshes it if already listed. */
  upsert(shard: Shard) {
    const i = this.shards.findIndex((s) => s.id === shard.id);
    if (i >= 0) this.shards[i] = shard;
    else this.shards.unshift(shard);
  }

  #replace(oldId: string, next: Shard) {
    const i = this.shards.findIndex((s) => s.id === oldId);
    if (i < 0) return;
    if (next.id !== oldId) {
      this.#keys.set(next.id, this.key(oldId));
      this.#keys.delete(oldId);
    }
    this.shards[i] = next;
  }

  /** Throws an error code ("exists", "invalid_name"…) on failure. */
  async rename(id: string, title: string) {
    const shard = this.find(id);
    if (!shard || title === (shard.title ?? '')) return;
    if (this.demo) {
      this.#replace(id, { ...shard, title });
      return;
    }
    this.#replace(id, await api.renameShard(id, title).catch(rethrow));
  }

  async toggleFavorite(id: string) {
    const shard = this.find(id);
    if (!shard) return;
    const favorite = !shard.favorite;
    shard.favorite = favorite;
    if (this.demo) return;
    try {
      this.#replace(id, await api.setFavorite(id, favorite));
    } catch (e) {
      shard.favorite = !favorite;
      rethrow(e);
    }
  }

  async remove(ids: string[]) {
    if (!this.demo) await api.deleteShards(ids).catch(rethrow);
    this.shards = this.shards.filter((s) => !ids.includes(s.id));
  }
}

function rethrow(e: unknown): never {
  throw errorCode(e);
}

export const library = new Library();
