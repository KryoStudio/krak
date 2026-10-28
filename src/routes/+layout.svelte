<!-- Shared by every window (main app and the in-game overlay): fonts, theme, language. -->
<script lang="ts">
  import '@fontsource-variable/inter';
  import '@fontsource-variable/unbounded';
  import '../app.css';
  import { onMount } from 'svelte';
  import { i18n, systemLang } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';

  let { children } = $props();

  $effect(() => {
    i18n.lang = settings.value.language ?? systemLang;
    document.documentElement.lang = i18n.lang;
  });
  $effect(() => {
    document.documentElement.dataset.accent = settings.value.accent;
  });

  onMount(() => {
    settings.load();
  });
</script>

{#if settings.loaded}
  {@render children()}
{/if}
