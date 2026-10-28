<!-- The "Shard captured" notification. -->
<script lang="ts">
  import { fly } from 'svelte/transition';
  import Logo from './Logo.svelte';
  import { t, formatDuration } from '$lib/i18n.svelte';

  let { game, duration, onclose }: { game: string; duration: number; onclose: () => void } = $props();

  $effect(() => {
    const timer = setTimeout(onclose, 5000);
    return () => clearTimeout(timer);
  });
</script>

<div class="toast" role="status" transition:fly={{ y: 24, duration: 350 }}>
  <div class="mark"><Logo size={30} /></div>
  <div class="text">
    <strong>{t('toast.title')}</strong>
    <span>{game} · {formatDuration(duration)}</span>
  </div>
  <button onclick={onclose}>{t('toast.open')}</button>
  <div class="timer"></div>
</div>

<style>
  .toast {
    position: fixed;
    right: 24px;
    bottom: 24px;
    z-index: 10;
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 320px;
    padding: 14px 16px;
    border-radius: 16px;
    background: rgba(21, 29, 53, 0.92);
    backdrop-filter: blur(12px);
    border: 1px solid rgba(79, 195, 247, 0.4);
    box-shadow:
      0 0 40px rgba(79, 195, 247, 0.18),
      0 20px 50px -20px rgba(0, 0, 0, 0.8);
    overflow: hidden;
  }
  .mark {
    filter: drop-shadow(0 0 8px rgba(79, 195, 247, 0.6));
    animation: pop 0.5s var(--ease);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
  }
  strong {
    font-weight: 600;
  }
  span {
    font-size: 13px;
    color: var(--gris);
  }
  button {
    font-size: 13px;
    font-weight: 600;
    padding: 7px 14px;
    border-radius: 9px;
    background: var(--cyan);
    color: var(--nuit);
  }
  .timer {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    background: var(--cyan);
    transform-origin: left;
    animation: drain 5s linear forwards;
  }
  @keyframes drain {
    to {
      transform: scaleX(0);
    }
  }
  @keyframes pop {
    from {
      transform: scale(0.4) rotate(-20deg);
    }
  }
</style>
