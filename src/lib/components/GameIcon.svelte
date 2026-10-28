<!-- A game's tile: its tint and initials. null = desktop. -->
<script lang="ts">
  import Icon from './Icon.svelte';
  import { gameInitials, gameTint } from '$lib/games';

  let { game, size = 28 }: { game: string | null; size?: number } = $props();

  const tint = $derived(gameTint(game));
</script>

<span
  class="tile"
  class:desktop={game === null}
  style:--a={tint[0]}
  style:--b={tint[1]}
  style:width="{size}px"
  style:height="{size}px"
  style:font-size="{Math.round(size * 0.36)}px"
>
  {#if game === null}
    <Icon name="monitor" size={Math.round(size * 0.55)} />
  {:else if size >= 18}
    {gameInitials(game)}
  {/if}
</span>

<style>
  .tile {
    display: inline-grid;
    place-items: center;
    flex: none;
    border-radius: 28%;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: rgba(255, 255, 255, 0.95);
    background: linear-gradient(135deg, var(--a), var(--b));
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.12);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.3);
  }
  .desktop {
    background: var(--surface-3);
    color: var(--text-2);
  }
</style>
