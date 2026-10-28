<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import ShardCard from '$lib/components/ShardCard.svelte';
  import Toast from '$lib/components/Toast.svelte';
  import { games, shards, type GameId } from '$lib/demo';
  import { t, formatSize } from '$lib/i18n.svelte';
  import { ui } from '$lib/ui.svelte';

  const chips: ('all' | GameId)[] = ['all', 'gta', 'rl', 'fortnite'];

  const visible = $derived(
    shards.filter((s) => {
      if (ui.filter === 'favorites' && !s.favorite) return false;
      if (ui.filter !== 'all' && ui.filter !== 'favorites' && s.game !== ui.filter) return false;
      const q = ui.query.trim().toLowerCase();
      return !q || (s.title ?? games[s.game].name).toLowerCase().includes(q);
    }),
  );
  const totalMb = $derived(visible.reduce((sum, s) => sum + s.sizeMb, 0));

  // Preview only: the hotkey shows the notification until the capture engine exists.
  let toast = $state<{ game: string; duration: number } | null>(null);
  function onkeydown(e: KeyboardEvent) {
    if (e.key !== ui.hotkey) return;
    e.preventDefault();
    const pool: GameId[] = ['gta', 'rl', 'fortnite'];
    toast = null;
    queueMicrotask(() => {
      toast = { game: games[pool[Math.floor(Math.random() * pool.length)]].name, duration: ui.bufferSeconds };
    });
  }
</script>

<svelte:window {onkeydown} />

<div class="frost"></div>

<section>
  <header>
    <div>
      <h1>{t('glacier.title')}</h1>
      <p class="count">{t('glacier.count', { count: visible.length, size: formatSize(totalMb) })}</p>
    </div>
    <span class="preview">{t('glacier.preview')}</span>
  </header>

  <div class="toolbar">
    <label class="search">
      <Icon name="search" size={16} />
      <input placeholder={t('glacier.search')} bind:value={ui.query} />
    </label>
    <div class="chips">
      {#each chips as c (c)}
        <button class:on={ui.filter === c} onclick={() => (ui.filter = c)}>
          {c === 'all' ? t('glacier.all') : games[c].name}
        </button>
      {/each}
    </div>
  </div>

  {#if visible.length}
    <div class="grid">
      {#each visible as shard (shard.id)}
        <ShardCard {shard} />
      {/each}
    </div>
  {:else}
    <p class="empty">{t('glacier.empty')}</p>
  {/if}
</section>

{#if toast}
  <Toast game={toast.game} duration={toast.duration} onclose={() => (toast = null)} />
{/if}

<style>
  .frost {
    position: absolute;
    inset: 0 0 auto 0;
    height: 320px;
    pointer-events: none;
    background:
      radial-gradient(60% 100% at 80% 0%, rgba(79, 195, 247, 0.1), transparent 70%),
      linear-gradient(rgba(154, 168, 194, 0.05) 1px, transparent 1px) 0 0 / 44px 44px,
      linear-gradient(90deg, rgba(154, 168, 194, 0.05) 1px, transparent 1px) 0 0 / 44px 44px;
    mask-image: linear-gradient(to bottom, #000, transparent);
  }

  section {
    position: relative;
    padding: 34px 40px 48px;
    max-width: 1500px;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
  }
  h1 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 800;
    font-size: 34px;
    letter-spacing: -0.02em;
  }
  .count {
    margin: 8px 0 0;
    color: var(--gris);
  }
  .preview {
    font-size: 12px;
    font-weight: 600;
    padding: 5px 12px;
    border-radius: 999px;
    color: var(--glace);
    border: 1px dashed rgba(184, 236, 251, 0.4);
  }

  .toolbar {
    display: flex;
    gap: 16px;
    align-items: center;
    margin: 28px 0 24px;
    flex-wrap: wrap;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 320px;
    padding: 10px 14px;
    border-radius: 12px;
    background: var(--nuit-2);
    border: 1px solid var(--line);
    color: var(--gris);
    transition: border-color 0.15s;
  }
  .search:focus-within {
    border-color: rgba(79, 195, 247, 0.6);
  }
  .search input {
    flex: 1;
    border: 0;
    outline: 0;
    background: none;
    color: var(--givre);
  }
  .search input::placeholder {
    color: var(--gris-2);
  }

  .chips {
    display: flex;
    gap: 8px;
  }
  .chips button {
    padding: 8px 14px;
    border-radius: 999px;
    font-size: 14px;
    font-weight: 500;
    color: var(--gris);
    border: 1px solid var(--line);
    transition: all 0.15s;
  }
  .chips button:hover {
    color: var(--givre);
    border-color: var(--line-strong);
  }
  .chips button.on {
    color: var(--nuit);
    background: var(--cyan);
    border-color: var(--cyan);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 20px;
  }
  .empty {
    color: var(--gris);
    margin-top: 60px;
    text-align: center;
  }
</style>
