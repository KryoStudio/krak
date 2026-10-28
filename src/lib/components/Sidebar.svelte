<script lang="ts">
  import Logo from './Logo.svelte';
  import Icon from './Icon.svelte';
  import { games, shards, type GameId } from '$lib/demo';
  import { i18n, t, type Lang } from '$lib/i18n.svelte';
  import { ui } from '$lib/ui.svelte';

  const gameIds: GameId[] = ['gta', 'rl', 'fortnite', 'desktop'];
  const countFor = (id: GameId) => shards.filter((s) => s.game === id).length;
  const gameName = (id: GameId) => (id === 'desktop' ? t('game.desktop') : games[id].name);
  const langs: Lang[] = ['fr', 'en'];
</script>

<aside>
  <div class="brand">
    <Logo size={34} />
    <div>
      <div class="kryo">KRYO</div>
      <div class="shard">SHARD</div>
    </div>
  </div>

  <div class="capture">
    <div class="status"><span class="dot"></span>{t('capture.active')}</div>
    <p>{t('capture.hint', { seconds: ui.bufferSeconds })}</p>
    <p class="key"><kbd>{ui.hotkey}</kbd> {t('capture.key')}</p>
  </div>

  <nav>
    <button class:active={ui.filter === 'all'} onclick={() => (ui.filter = 'all')}>
      <Icon name="grid" />{t('nav.glacier')}
    </button>
    <button class:active={ui.filter === 'favorites'} onclick={() => (ui.filter = 'favorites')}>
      <Icon name="star" />{t('nav.favorites')}
    </button>

    <div class="section">{t('nav.games')}</div>
    {#each gameIds as id (id)}
      <button class:active={ui.filter === id} onclick={() => (ui.filter = id)}>
        {#if id === 'desktop'}
          <Icon name="monitor" />
        {:else}
          <span class="swatch" style:background="linear-gradient(135deg, {games[id].tint[0]}, {games[id].tint[1]})"></span>
        {/if}
        <span class="label">{gameName(id)}</span>
        <span class="count">{countFor(id)}</span>
      </button>
    {/each}
  </nav>

  <div class="foot">
    <button class="settings"><Icon name="sliders" />{t('nav.settings')}</button>
    <div class="meta">
      <span>v0.1.0 · {t('about.by')}</span>
      <span class="langs">
        {#each langs as l (l)}
          <button class:on={i18n.lang === l} onclick={() => (i18n.lang = l)}>{l.toUpperCase()}</button>
        {/each}
      </span>
    </div>
  </div>
</aside>

<style>
  aside {
    display: flex;
    flex-direction: column;
    gap: 22px;
    padding: 22px 16px 16px;
    background: var(--nuit-2);
    border-right: 1px solid var(--line);
    height: 100vh;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 8px;
  }
  .kryo {
    font-family: var(--font-display);
    font-weight: 800;
    font-size: 19px;
    line-height: 0.9;
  }
  .shard {
    font-family: var(--font-display);
    font-weight: 500;
    font-size: 8.5px;
    letter-spacing: 0.5em;
    color: var(--cyan);
    margin-top: 5px;
  }

  .capture {
    position: relative;
    padding: 14px 16px;
    border-radius: var(--radius);
    background: linear-gradient(160deg, rgba(79, 195, 247, 0.14), rgba(79, 195, 247, 0.03));
    border: 1px solid rgba(79, 195, 247, 0.28);
    overflow: hidden;
  }
  .capture::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(105deg, transparent 40%, rgba(184, 236, 251, 0.12) 50%, transparent 60%);
    transform: translateX(-100%);
    animation: sheen 6s var(--ease) infinite;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 9px;
    font-weight: 600;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--cyan);
    box-shadow: 0 0 0 0 rgba(79, 195, 247, 0.6);
    animation: pulse 2s infinite;
  }
  .capture p {
    margin: 8px 0 0;
    font-size: 13px;
    color: var(--gris);
    line-height: 1.45;
  }
  .capture .key {
    color: var(--givre);
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    overflow-y: auto;
  }
  nav button,
  .settings {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 9px 10px;
    border-radius: 10px;
    color: var(--gris);
    font-weight: 500;
    text-align: left;
    transition: background 0.15s, color 0.15s;
  }
  nav button:hover,
  .settings:hover {
    background: rgba(154, 168, 194, 0.07);
    color: var(--givre);
  }
  nav button.active {
    background: var(--cyan-soft);
    color: var(--givre);
    box-shadow: inset 2px 0 0 var(--cyan);
  }
  nav button.active :global(svg) {
    color: var(--cyan);
  }
  .section {
    margin: 18px 10px 6px;
    font-family: var(--font-display);
    font-size: 10px;
    font-weight: 500;
    letter-spacing: 0.3em;
    text-transform: uppercase;
    color: var(--gris-2);
  }
  .swatch {
    width: 18px;
    height: 18px;
    border-radius: 6px;
    flex: none;
  }
  .label {
    flex: 1;
  }
  .count {
    font-size: 12px;
    color: var(--gris-2);
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: 10px;
    border-top: 1px solid var(--line);
    padding-top: 12px;
  }
  .meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0 10px;
    font-size: 11.5px;
    color: var(--gris-2);
  }
  .langs {
    display: flex;
    gap: 2px;
  }
  .langs button {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 6px;
    border-radius: 6px;
    color: var(--gris-2);
  }
  .langs button.on {
    color: var(--nuit);
    background: var(--cyan);
  }

  @keyframes pulse {
    70% {
      box-shadow: 0 0 0 8px rgba(79, 195, 247, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(79, 195, 247, 0);
    }
  }
  @keyframes sheen {
    0%,
    60% {
      transform: translateX(-100%);
    }
    100% {
      transform: translateX(100%);
    }
  }
</style>
