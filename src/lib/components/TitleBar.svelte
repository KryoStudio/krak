<!-- Custom title bar: the window has no native frame. Empty areas drag the window. -->
<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import Icon from './Icon.svelte';
  import { native } from '$lib/api';
  import { t } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';
  import { tooltip } from '$lib/tooltip';

  const win = native ? getCurrentWindow() : null;
  let maximized = $state(false);

  $effect(() => {
    if (!win) return;
    let stop: (() => void) | undefined;
    let alive = true;
    const sync = async () => (maximized = await win.isMaximized());
    sync();
    win.onResized(sync).then((unlisten) => (alive ? (stop = unlisten) : unlisten()));
    return () => {
      alive = false;
      stop?.();
    };
  });

  const collapsed = $derived(settings.value.sidebarCollapsed);
  const toggleLabel = $derived(t(collapsed ? 'nav.expand' : 'nav.collapse'));
</script>

<header data-tauri-drag-region>
  <button
    class="toggle"
    onclick={() => settings.update({ sidebarCollapsed: !collapsed }).catch(() => {})}
    aria-label={toggleLabel}
    aria-expanded={!collapsed}
    use:tooltip={{ text: `${toggleLabel}  ·  Ctrl+B`, side: 'bottom' }}
  >
    <Icon name="sidebar" size={17} />
  </button>

  {#if win}
    <div class="controls">
      <button onclick={() => win.minimize()} aria-label={t('window.minimize')}>
        <svg viewBox="0 0 10 10" width="10" height="10"><path d="M0 5.5h10" /></svg>
      </button>
      <button onclick={() => win.toggleMaximize()} aria-label={t(maximized ? 'window.restore' : 'window.maximize')}>
        {#if maximized}
          <svg viewBox="0 0 10 10" width="10" height="10">
            <rect x="0.5" y="2.5" width="7" height="7" rx="1" />
            <path d="M2.5 2.5v-1a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" />
          </svg>
        {:else}
          <svg viewBox="0 0 10 10" width="10" height="10"><rect x="0.5" y="0.5" width="9" height="9" rx="1.5" /></svg>
        {/if}
      </button>
      <button class="close" onclick={() => win.close()} aria-label={t('window.close')}>
        <svg viewBox="0 0 10 10" width="10" height="10"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
      </button>
    </div>
  {/if}
</header>

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--titlebar);
    padding-left: 10px;
    flex: none;
  }

  .toggle {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    color: var(--text-3);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .toggle:hover {
    background: rgba(255, 255, 255, 0.05);
    color: var(--text);
  }

  .controls {
    display: flex;
    align-self: stretch;
  }
  .controls button {
    display: grid;
    place-items: center;
    width: 46px;
    color: var(--text-2);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .controls svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
  }
  .controls button:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .controls .close:hover {
    background: #c42b1c;
    color: #fff;
  }
</style>
