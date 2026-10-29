<!-- Shared by every window (main app and the in-game overlay): fonts, theme, language. -->
<script lang="ts">
  import '@fontsource-variable/space-grotesk';
  import '../app.css';
  import { onMount } from 'svelte';
  import { i18n, locale, resolveLang } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';

  let { children } = $props();

  $effect(() => {
    i18n.lang = resolveLang(settings.value.language);
    document.documentElement.lang = locale();
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
