<!-- Screenshots: a grid filtered by game, a full-size viewer (←/→), copy for Discord, delete. -->
<script lang="ts">
  import { flip } from 'svelte/animate';
  import { fade, fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import { SvelteSet } from 'svelte/reactivity';
  import Icon from '$lib/components/Icon.svelte';
  import GameIcon from '$lib/components/GameIcon.svelte';
  import Select from '$lib/components/Select.svelte';
  import SelectionBar from '$lib/components/SelectionBar.svelte';
  import { mediaSrc, type Screenshot } from '$lib/api';
  import { gameTint } from '$lib/games';
  import { formatAge, formatHotkey, formatSize, t } from '$lib/i18n.svelte';
  import { portal } from '$lib/portal';
  import { screens } from '$lib/screenshots.svelte';
  import { settings } from '$lib/settings.svelte';
  import { showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { confirm, openMenu, toast, ui } from '$lib/ui.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);

  /** undefined = all games, null = desktop. */
  let game = $state<string | null | undefined>(undefined);
  const shown = $derived(
    screens.items.filter((s) => game === undefined || s.game === game).sort((a, b) => b.createdAt - a.createdAt),
  );
  const size = $derived(shown.reduce((sum, s) => sum + s.size, 0));
  const viewing = $derived(ui.viewing ? screens.find(ui.viewing) : undefined);
  const index = $derived(viewing ? shown.indexOf(viewing) : -1);

  const gameOptions = $derived([
    { value: undefined as string | null | undefined, label: t('screens.allGames') },
    ...screens.games.map((g) => ({ value: g as string | null | undefined, label: g ?? t('game.desktop') })),
  ]);

  // Selection ----------------------------------------------------------------------
  const selection = new SvelteSet<string>();
  let anchor: string | null = null;

  function select(shot: Screenshot, shift: boolean) {
    if (shift && anchor) {
      const a = shown.findIndex((s) => s.id === anchor);
      const b = shown.findIndex((s) => s.id === shot.id);
      if (a >= 0 && b >= 0) {
        for (const s of shown.slice(Math.min(a, b), Math.max(a, b) + 1)) selection.add(s.id);
        return;
      }
    }
    if (selection.has(shot.id)) selection.delete(shot.id);
    else selection.add(shot.id);
    anchor = shot.id;
  }

  $effect(() => {
    void game;
    selection.clear();
  });
  $effect(() => {
    for (const id of selection) if (!screens.find(id)) selection.delete(id);
  });

  async function remove(ids: string[]) {
    const many = ids.length > 1;
    const ok = await confirm({
      title: many ? t('screens.deleteTitleMany', { count: ids.length }) : t('screens.deleteTitle'),
      body: many ? t('screens.deleteBodyMany', { count: ids.length }) : t('screens.deleteBody'),
      confirm: t('shard.delete'),
      danger: true,
    });
    if (!ok) return;
    try {
      await screens.remove(ids);
      selection.clear();
      if (ui.viewing && ids.includes(ui.viewing)) ui.viewing = null;
      toast({ kind: 'info', title: t('toast.deleted') });
    } catch (e) {
      showError(e);
    }
  }

  function open(shot: Screenshot, e: MouseEvent) {
    if (selection.size || e.ctrlKey || e.shiftKey) select(shot, e.shiftKey);
    else ui.viewing = shot.id;
  }

  function menuFor(shot: Screenshot, x: number, y: number) {
    openMenu(x, y, [
      { label: t('screens.copy'), icon: 'copy', shortcut: 'Ctrl+C', run: () => screens.copy(shot) },
      { label: t('shard.reveal'), icon: 'folder', run: () => screens.reveal(shot) },
      'separator',
      { label: t('shard.delete'), icon: 'trash', shortcut: t('key.delete'), danger: true, run: () => remove([shot.id]) },
    ]);
  }

  function go(step: number) {
    const next = shown[index + step];
    if (next) ui.viewing = next.id;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || (e.target as HTMLElement).closest('input')) return;
    const key = e.key;
    if (viewing) {
      if (key === 'Escape') ui.viewing = null;
      else if (key === 'ArrowLeft') go(-1);
      else if (key === 'ArrowRight') go(1);
      else if (key === 'Delete') remove([viewing.id]);
      else if (e.ctrlKey && key.toLowerCase() === 'c') screens.copy(viewing);
      else return;
      e.preventDefault();
    } else if (e.ctrlKey && key.toLowerCase() === 'a') {
      e.preventDefault();
      for (const s of shown) selection.add(s.id);
    } else if (key === 'Escape' && selection.size) selection.clear();
    else if (key === 'Delete' && selection.size) remove([...selection]);
  }

  const src = (shot: Screenshot) => (shot.path ? mediaSrc(shot.path) : null);
</script>

<svelte:window {onkeydown} />

<header>
  <div>
    <h1>{t('nav.screenshots')}</h1>
    <p class="count">{t('screens.count', { count: shown.length })} · {formatSize(size)}</p>
  </div>
  {#if screens.games.length > 1}
    <Select label={t('screens.allGames')} options={gameOptions} value={game} onchange={(v) => (game = v)} width={200} />
  {/if}
</header>

<section>
  {#if screens.loading}
    <!-- near instant -->
  {:else if shown.length}
    <div class="grid">
      {#each shown as shot, i (shot.id)}
        {@const tint = gameTint(shot.game)}
        <div
          class="shot"
          class:selected={selection.has(shot.id)}
          animate:flip={{ duration: 300 * motion, easing: cubicOut }}
          in:scale={{ start: 0.96, opacity: 0, duration: 260 * motion, delay: Math.min(i, 12) * 20 * motion }}
          oncontextmenu={(e) => (e.preventDefault(), menuFor(shot, e.clientX, e.clientY))}
          role="presentation"
        >
          <button class="image" style:--a={tint[0]} style:--b={tint[1]} onclick={(e) => open(shot, e)} aria-label={shot.game ?? t('game.desktop')}>
            {#if src(shot)}
              <img src={src(shot)} alt="" loading="lazy" decoding="async" />
            {:else}
              <span class="art"></span>
            {/if}
            <span class="shade"></span>
            <span class="meta">
              <GameIcon game={shot.game} size={18} />
              <span>{shot.game ?? t('game.desktop')} · {formatAge(shot.createdAt)}</span>
            </span>
          </button>
          <button
            class="check"
            class:on={selection.has(shot.id)}
            class:visible={selection.size > 0}
            onclick={(e) => select(shot, e.shiftKey)}
            role="checkbox"
            aria-checked={selection.has(shot.id)}
            aria-label={t('selection.select')}
          >
            <Icon name="check" size={13} stroke={3} />
          </button>
          <div class="actions" class:hidden={selection.size > 0}>
            <button onclick={(e) => menuFor(shot, e.clientX, e.clientY)} aria-label={t('shard.more')}><Icon name="more" size={16} stroke={2.4} /></button>
            <button onclick={() => screens.copy(shot)} aria-label={t('screens.copy')} use:tooltip={{ text: t('screens.copy'), side: 'bottom' }}>
              <Icon name="copy" size={14} />
            </button>
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="empty" in:fly={{ y: 10, duration: 300 * motion }}>
      <div class="mark"><Icon name="camera" size={30} /></div>
      <h2>{t('screens.emptyTitle')}</h2>
      <p>{t('screens.emptyBody')}</p>
      <p class="keys">{#each formatHotkey(settings.value.screenshotHotkey) as k, i (i)}{#if i > 0}<span class="plus">+</span>{/if}<kbd>{k}</kbd>{/each}</p>
    </div>
  {/if}
</section>

{#if selection.size}
  <SelectionBar
    count={selection.size}
    total={shown.length}
    onall={() => shown.forEach((s) => selection.add(s.id))}
    ondelete={() => remove([...selection])}
    onclear={() => selection.clear()}
  />
{/if}

{#if viewing}
  {@const tint = gameTint(viewing.game)}
  <div class="viewer" use:portal={'.content'} transition:fade={{ duration: 160 * motion }}>
    <div class="viewer-bar">
      <button class="icon-btn" onclick={() => (ui.viewing = null)} aria-label={t('common.close')}><Icon name="chevronLeft" size={18} /></button>
      <GameIcon game={viewing.game} size={28} />
      <div class="viewer-title">
        <strong>{viewing.game ?? t('game.desktop')}</strong>
        <span>{formatAge(viewing.createdAt)} · {viewing.width}×{viewing.height} · {formatSize(viewing.size)}</span>
      </div>
      <button class="icon-btn" onclick={() => viewing && screens.copy(viewing)} aria-label={t('screens.copy')} use:tooltip={{ text: `${t('screens.copy')} · Ctrl+C`, side: 'bottom' }}>
        <Icon name="copy" size={16} />
      </button>
      <button class="icon-btn" onclick={() => viewing && screens.reveal(viewing)} aria-label={t('shard.reveal')} use:tooltip={{ text: t('shard.reveal'), side: 'bottom' }}>
        <Icon name="folder" size={16} />
      </button>
      <button class="icon-btn danger" onclick={() => viewing && remove([viewing.id])} aria-label={t('shard.delete')} use:tooltip={{ text: t('shard.delete'), side: 'bottom' }}>
        <Icon name="trash" size={16} />
      </button>
    </div>
    <div class="stage">
      <button class="nav" disabled={index <= 0} onclick={() => go(-1)} aria-label={t('player.prev')}><Icon name="chevronLeft" size={22} /></button>
      {#key viewing.id}
        <div class="frame" in:scale={{ start: 0.985, duration: 220 * motion, opacity: 0 }}>
          {#if src(viewing)}
            <img src={src(viewing)} alt="" />
          {:else}
            <div class="placeholder" style:--a={tint[0]} style:--b={tint[1]}></div>
          {/if}
        </div>
      {/key}
      <button class="nav" disabled={index < 0 || index >= shown.length - 1} onclick={() => go(1)} aria-label={t('player.next')}><Icon name="chevronRight" size={22} /></button>
    </div>
  </div>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 32px 18px;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.015em;
  }
  .count {
    margin-top: 2px;
    font-size: 13px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  section {
    padding: 6px 32px 40px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 14px;
  }

  .shot {
    position: relative;
    border-radius: var(--radius-lg);
    transition: transform var(--slow) var(--ease-out);
  }
  .image {
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 16 / 9;
    border-radius: var(--radius-lg);
    overflow: hidden;
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--border);
    isolation: isolate;
    transition:
      box-shadow var(--normal),
      transform var(--slow) var(--ease-out);
  }
  .image img,
  .art {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    transition: transform 600ms var(--ease-out);
  }
  .art {
    background:
      linear-gradient(115deg, transparent 47%, rgba(255, 255, 255, 0.06) 47.2%, transparent 62%),
      radial-gradient(110% 90% at 88% 8%, color-mix(in srgb, var(--b) 55%, transparent), transparent 60%),
      radial-gradient(100% 100% at 8% 100%, color-mix(in srgb, var(--a) 60%, transparent), transparent 65%),
      linear-gradient(135deg, var(--surface-2), var(--bg));
  }
  .shot:hover .image img,
  .shot:hover .art {
    transform: scale(1.035);
  }
  .shot:hover .image {
    box-shadow:
      inset 0 0 0 1px var(--border-strong),
      0 14px 30px -18px rgba(0, 0, 0, 0.9);
  }
  .shade {
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, rgba(0, 0, 0, 0.55), transparent 42%);
    opacity: 0;
    transition: opacity var(--normal);
  }
  .meta {
    position: absolute;
    left: 10px;
    bottom: 9px;
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
    font-weight: 500;
    color: #fff;
    opacity: 0;
    transform: translateY(4px);
    transition:
      opacity var(--normal),
      transform var(--slow) var(--ease-out);
  }
  .shot:hover :is(.shade, .meta) {
    opacity: 1;
    transform: none;
  }
  .shot.selected .image {
    transform: scale(0.94);
    box-shadow: 0 0 0 2px var(--accent);
  }

  .check {
    position: absolute;
    top: 9px;
    left: 9px;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 7px;
    color: transparent;
    background: rgba(10, 11, 13, 0.5);
    box-shadow: inset 0 0 0 1.5px rgba(255, 255, 255, 0.75);
    backdrop-filter: blur(8px);
    opacity: 0;
    transform: scale(0.85);
    transition:
      opacity var(--normal) var(--ease-out),
      transform var(--slow) var(--ease-out),
      background var(--fast),
      color var(--fast);
  }
  .shot:hover .check,
  .check.visible {
    opacity: 1;
    transform: none;
  }
  .check.on {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }
  .actions {
    position: absolute;
    top: 9px;
    right: 9px;
    display: flex;
    gap: 4px;
  }
  .actions.hidden {
    display: none;
  }
  .actions button {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    color: rgba(255, 255, 255, 0.9);
    background: rgba(10, 11, 13, 0.55);
    backdrop-filter: blur(8px);
    opacity: 0;
    transform: scale(0.9);
    transition:
      opacity var(--normal),
      transform var(--slow) var(--ease-out),
      background var(--fast);
  }
  .shot:hover .actions button {
    opacity: 1;
    transform: none;
  }
  .actions button:hover {
    background: rgba(10, 11, 13, 0.8);
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    margin-top: 12vh;
    text-align: center;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 80px;
    height: 80px;
    border-radius: 24px;
    color: var(--accent);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: 0 0 60px -10px color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .empty h2 {
    margin-top: 20px;
    font-size: 18px;
    font-weight: 600;
  }
  .empty p {
    max-width: 380px;
    margin-top: 6px;
    color: var(--text-2);
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 14px !important;
  }
  .plus {
    color: var(--text-3);
  }

  .viewer {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: color-mix(in srgb, var(--bg) 97%, transparent);
    backdrop-filter: blur(12px);
  }
  .viewer-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 20px 12px 16px;
  }
  .viewer-title {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .viewer-title strong {
    font-size: 15px;
    font-weight: 600;
  }
  .viewer-title span {
    font-size: 12.5px;
    color: var(--text-3);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    color: var(--text-2);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .icon-btn:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .icon-btn.danger:hover {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .stage {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px 24px;
  }
  .frame {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: grid;
    place-items: center;
  }
  .frame img,
  .placeholder {
    max-width: 100%;
    max-height: 100%;
    border-radius: var(--radius-lg);
    box-shadow: 0 30px 80px -30px rgba(0, 0, 0, 0.9);
  }
  .placeholder {
    width: 100%;
    aspect-ratio: 16 / 9;
    background:
      radial-gradient(110% 90% at 88% 8%, color-mix(in srgb, var(--b) 50%, transparent), transparent 60%),
      radial-gradient(100% 100% at 8% 100%, color-mix(in srgb, var(--a) 55%, transparent), transparent 65%),
      var(--surface);
  }
  .nav {
    display: grid;
    place-items: center;
    width: 40px;
    height: 64px;
    flex: none;
    border-radius: var(--radius);
    color: var(--text-2);
    transition:
      background var(--fast),
      opacity var(--fast);
  }
  .nav:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .nav:disabled {
    opacity: 0.25;
  }
</style>
