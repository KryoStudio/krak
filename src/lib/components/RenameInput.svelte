<!-- Inline title editor. Enter or leaving the field saves, Escape cancels. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import type { Shard } from '$lib/api';
  import { t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { gameName, showError } from '$lib/shard-actions';
  import { ui } from '$lib/ui.svelte';

  let { shard, size = 14, ondone }: { shard: Shard; size?: number; ondone?: () => void } = $props();

  let input: HTMLInputElement;
  // svelte-ignore state_referenced_locally
  let draft = $state(shard.title ?? '');

  onMount(() => {
    input.focus();
    input.select();
  });

  function finish() {
    ui.renaming = null;
    ondone?.();
  }

  function commit() {
    if (ui.renaming !== shard.id) return;
    const value = draft.trim();
    const id = shard.id;
    finish();
    if (value && value !== shard.title) library.rename(id, value).catch(showError);
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Enter') commit();
    else if (e.key === 'Escape') finish();
  }
</script>

<input
  bind:this={input}
  bind:value={draft}
  style:font-size="{size}px"
  placeholder={gameName(shard)}
  maxlength="120"
  spellcheck="false"
  aria-label={t('shard.rename')}
  {onkeydown}
  onblur={commit}
/>

<style>
  input {
    display: block;
    width: calc(100% + 12px);
    height: 1.75em;
    margin: 0 -6px;
    padding: 0 5px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--accent-line);
    background: var(--bg);
    box-shadow: 0 0 0 3px var(--accent-soft);
    font-weight: 600;
    outline: none;
  }
  input::placeholder {
    color: var(--text-3);
  }
</style>
