<!-- The main window: sidebar, title bar, pages, and everything floating above them. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import TitleBar from '$lib/components/TitleBar.svelte';
  import Player from '$lib/components/Player.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import Toasts from '$lib/components/Toasts.svelte';
  import { native, on } from '$lib/api';
  import { capture, saveReplay } from '$lib/capture.svelte';
  import { hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import { announce } from '$lib/shard-actions';
  import { toast, ui } from '$lib/ui.svelte';
  import { updates } from '$lib/updates.svelte';

  let { children } = $props();

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);

  $effect(() => {
    void page.url.pathname;
    ui.playing = null;
  });

  onMount(() => {
    library.load().catch(console.error);
    if (!native) return;

    capture.refresh().catch(console.error);
    // Look for an update once things have settled.
    const updateCheck = setTimeout(() => updates.check(true), 8000);
    // Buffer fill and memory move every second; only ask while someone is looking.
    const poll = setInterval(() => {
      if (document.visibilityState === 'visible' && capture.status.state !== 'paused') capture.refresh().catch(() => {});
    }, 1000);

    let reload: ReturnType<typeof setTimeout>;
    const listeners = [
      on('library-changed', () => {
        if (library.demo) return;
        clearTimeout(reload);
        reload = setTimeout(() => library.load().catch(console.error), 150);
      }),
      on('capture-status', (status) => {
        capture.status = status;
      }),
      on('shard-saved', announce),
      on('capture-failed', (code) => {
        const key = `capture.failed.${code}`;
        toast({ kind: 'error', title: t(hasKey(key) ? key : 'capture.failed.encode_failed') });
      }),
      on('navigate', (route) => goto(route)),
    ];
    return () => {
      clearInterval(poll);
      clearTimeout(updateCheck);
      listeners.forEach((l) => l.then((unlisten) => unlisten()));
    };
  });

  function onkeydown(e: KeyboardEvent) {
    const key = e.key.toLowerCase();
    if (e.ctrlKey && key === 'b') {
      e.preventDefault();
      settings.update({ sidebarCollapsed: !settings.value.sidebarCollapsed }).catch(() => {});
    } else if (e.ctrlKey && key === ',') {
      e.preventDefault();
      goto('/settings');
    } else if (!native && e.key === settings.value.hotkey) {
      // Browser preview: the global hotkey doesn't exist, the page's key does the same.
      e.preventDefault();
      saveReplay();
    } else if (!import.meta.env.DEV && (e.key === 'F5' || (e.ctrlKey && ['r', 'p', 'u', 'g', 'j'].includes(key)))) {
      // A desktop app, not a web page: no reload, print or view-source.
      e.preventDefault();
    }
  }

  function oncontextmenu(e: MouseEvent) {
    const editable = (e.target as HTMLElement).closest('input, textarea');
    if (!editable && !import.meta.env.DEV) e.preventDefault();
  }
</script>

<svelte:window {onkeydown} {oncontextmenu} />

<div class="app">
  <Sidebar />
  <div class="column">
    <TitleBar />
    <div class="content">
      <main>
        {#key page.url.pathname}
          <div class="route" in:fly={{ y: 8, duration: 280 * motion, easing: cubicOut }}>
            {@render children()}
          </div>
        {/key}
      </main>
      <Player />
    </div>
  </div>
</div>

<ContextMenu />
<ConfirmDialog />
<Toasts />

<style>
  .app {
    display: flex;
    height: 100vh;
  }
  .column {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .content {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  main {
    height: 100%;
    overflow-y: auto;
    scrollbar-gutter: stable;
  }
</style>
