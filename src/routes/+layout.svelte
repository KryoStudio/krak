<script lang="ts">
  import '@fontsource-variable/space-grotesk';
  import '@fontsource-variable/unbounded';
  import '../app.css';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import { i18n } from '$lib/i18n.svelte';

  let { children } = $props();

  $effect(() => {
    document.documentElement.lang = i18n.lang;
  });
</script>

<div class="app">
  <Sidebar />
  <main>{@render children()}</main>
</div>

<style>
  .app {
    display: grid;
    grid-template-columns: 260px 1fr;
    height: 100vh;
  }
  main {
    position: relative;
    overflow-y: auto;
  }
</style>
