<script lang="ts">
  import { flip } from 'svelte/animate';
  import { fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from '$lib/components/Icon.svelte';
  import GameIcon from '$lib/components/GameIcon.svelte';
  import Logo from '$lib/components/Logo.svelte';
  import ShardCard from '$lib/components/ShardCard.svelte';
  import { api, native } from '$lib/api';
  import { capture, saveReplay } from '$lib/capture.svelte';
  import { formatHotkey, formatSize, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import { tooltip } from '$lib/tooltip';
  import { openMenu, ui, type Sort } from '$lib/ui.svelte';
  import { view } from '$lib/view.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const sorts: Sort[] = ['recent', 'oldest', 'longest', 'largest', 'name'];

  let search: HTMLInputElement;
  let sortButton: HTMLButtonElement;

  // The header sticks to the top; it gets a background once the grid scrolls under it.
  let sentinel: HTMLElement;
  let stuck = $state(false);
  $effect(() => {
    const io = new IntersectionObserver(([e]) => (stuck = !e.isIntersecting), { root: sentinel.closest('main') });
    io.observe(sentinel);
    return () => io.disconnect();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.ctrlKey && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      search.focus();
      search.select();
    }
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      ui.query = '';
      search.blur();
    }
  }

  function openSort() {
    const r = sortButton.getBoundingClientRect();
    openMenu(
      r.left,
      r.bottom + 6,
      sorts.map((s) => ({ label: t(`glacier.sort.${s}`), icon: ui.sort === s ? 'check' : undefined, run: () => (ui.sort = s) })),
    );
  }

  const emptyMessage = $derived(
    ui.query.trim()
      ? t('empty.search', { query: ui.query.trim() })
      : ui.filter === 'favorites'
        ? t('empty.favorites')
        : t('empty.game'),
  );
</script>

<svelte:window {onkeydown} />

<div class="sentinel" bind:this={sentinel}></div>

<header class:stuck>
  {#key ui.filter}
    <div class="heading" in:fly={{ y: 6, duration: 240 * motion, easing: cubicOut }}>
      {#if view.game !== undefined}<GameIcon game={view.game} size={34} />{/if}
      <div>
        <h1>{view.title}</h1>
        <p class="count">{t('glacier.count', { count: view.shards.length, size: formatSize(view.size) })}</p>
      </div>
    </div>
  {/key}

  <div class="tools">
    {#if library.demo}<span class="demo">{t('glacier.demo')}</span>{/if}

    <label class="search">
      <Icon name="search" size={15} />
      <input bind:this={search} bind:value={ui.query} placeholder={t('glacier.search')} spellcheck="false" onkeydown={onSearchKey} />
      {#if ui.query}
        <button class="clear" onclick={() => ((ui.query = ''), search.focus())} aria-label={t('common.close')}>
          <Icon name="x" size={13} />
        </button>
      {:else}
        <kbd>Ctrl F</kbd>
      {/if}
    </label>

    <button class="tool" bind:this={sortButton} onclick={openSort} aria-haspopup="menu">
      <Icon name="sort" size={15} />
      <span>{t(`glacier.sort.${ui.sort}`)}</span>
      <Icon name="chevronDown" size={14} />
    </button>

    {#if native && !library.demo}
      <button class="tool square" onclick={() => api.openClipsDir()} aria-label={t('glacier.openFolder')} use:tooltip={{ text: t('glacier.openFolder'), side: 'bottom' }}>
        <Icon name="folder" size={16} />
      </button>
    {/if}

    <button
      class="capture"
      onclick={() => saveReplay()}
      disabled={!capture.live}
      use:tooltip={{ text: `${t('capture.nowHint', { seconds: settings.value.bufferSeconds })} · ${settings.value.hotkey}`, side: 'bottom' }}
    >
      <Icon name="record" size={15} />
      {t('capture.now')}
    </button>
  </div>
</header>

<section>
  {#if library.loading}
    <!-- nothing: loading is near instant, a skeleton would only flash -->
  {:else if view.shards.length}
    <div class="grid">
      {#each view.shards as shard, i (library.key(shard.id))}
        <div
          animate:flip={{ duration: 320 * motion, easing: cubicOut }}
          in:scale={{ start: 0.96, opacity: 0, duration: 280 * motion, delay: Math.min(i, 12) * 22 * motion, easing: cubicOut }}
        >
          <ShardCard {shard} fresh={ui.fresh === shard.id} />
        </div>
      {/each}
    </div>
  {:else if library.shards.length === 0}
    <div class="empty first" in:fly={{ y: 10, duration: 320 * motion }}>
      <div class="mark"><Logo size={44} /></div>
      <h2>{t('empty.title')}</h2>
      <p>
        {#each t('empty.body', { key: '\u0000', seconds: settings.value.bufferSeconds }).split('\u0000') as part, i (i)}
          {#if i > 0}{#each formatHotkey(settings.value.hotkey) as k (k)}<kbd>{k}</kbd>{/each}{/if}{part}
        {/each}
      </p>
      {#if native}
        <button class="tool" onclick={() => api.openClipsDir()}><Icon name="folder" size={15} />{t('glacier.openFolder')}</button>
      {/if}
    </div>
  {:else}
    <p class="empty" in:fly={{ y: 6, duration: 240 * motion }}>{emptyMessage}</p>
  {/if}
</section>

<style>
  .sentinel {
    height: 1px;
  }

  header {
    position: sticky;
    top: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 8px 32px 18px;
    margin-top: -1px;
    background: transparent;
    border-bottom: 1px solid transparent;
    transition:
      background var(--normal),
      border-color var(--normal),
      padding var(--normal) var(--ease-out);
  }
  header.stuck {
    padding-top: 10px;
    padding-bottom: 12px;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    backdrop-filter: blur(14px);
    border-bottom-color: var(--border);
  }

  .heading {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.015em;
    line-height: 1.2;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .count {
    margin-top: 2px;
    font-size: 13px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .demo {
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-3);
    border: 1px dashed var(--border-strong);
    margin-right: 4px;
  }

  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 260px;
    height: 34px;
    padding: 0 6px 0 11px;
    border-radius: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    color: var(--text-3);
    transition:
      border-color var(--fast),
      box-shadow var(--fast),
      width var(--slow) var(--ease-out);
  }
  .search:focus-within {
    width: 300px;
    border-color: var(--accent-line);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: none;
    font-size: 13px;
    color: var(--text);
  }
  .search input::placeholder {
    color: var(--text-3);
  }
  .search kbd {
    height: 18px;
    font-size: 10.5px;
    color: var(--text-3);
    background: none;
  }
  .clear {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 5px;
    color: var(--text-3);
  }
  .clear:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.06);
  }

  .tool {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 11px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-2);
    background: var(--surface);
    border: 1px solid var(--border);
    transition:
      background var(--fast),
      color var(--fast),
      border-color var(--fast);
  }
  .tool:hover {
    color: var(--text);
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  .capture {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 14px 0 12px;
    margin-left: 4px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: 0 6px 18px -8px var(--accent);
    transition:
      filter var(--fast),
      transform var(--fast) var(--ease-out),
      opacity var(--normal);
  }
  .capture:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .capture:active:not(:disabled) {
    transform: scale(0.97);
  }
  .capture:disabled {
    opacity: 0.4;
    box-shadow: none;
  }
  .tool.square {
    width: 34px;
    padding: 0;
    justify-content: center;
  }

  section {
    padding: 6px 32px 40px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr));
    gap: 18px;
  }

  .empty {
    margin-top: 80px;
    text-align: center;
    color: var(--text-3);
  }
  .empty.first {
    display: flex;
    flex-direction: column;
    align-items: center;
    margin-top: 12vh;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 84px;
    height: 84px;
    border-radius: 24px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: 0 0 60px -10px color-mix(in srgb, var(--face) 35%, transparent);
  }
  .empty h2 {
    margin-top: 22px;
    font-size: 18px;
    font-weight: 600;
    color: var(--text);
  }
  .empty.first p {
    max-width: 380px;
    margin: 8px 0 22px;
    line-height: 1.6;
    color: var(--text-2);
  }
  .empty kbd {
    margin: 0 2px;
  }
</style>
