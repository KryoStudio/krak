<!-- Notifications, bottom right. They wait while hovered. -->
<script lang="ts">
  import { flip } from 'svelte/animate';
  import { fly } from 'svelte/transition';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import Logo from './Logo.svelte';
  import { dismissToast, toasts, type Toast } from '$lib/ui.svelte';

  const lifetime = (t: Toast) => (t.kind === 'capture' ? 6000 : t.kind === 'error' ? 6000 : 4000);

  function autoDismiss(node: HTMLElement, toast: Toast) {
    let remaining = lifetime(toast);
    let started = Date.now();
    let timer = setTimeout(() => dismissToast(toast.id), remaining);
    const pause = () => {
      clearTimeout(timer);
      remaining -= Date.now() - started;
      node.classList.add('paused');
    };
    const resume = () => {
      started = Date.now();
      timer = setTimeout(() => dismissToast(toast.id), remaining);
      node.classList.remove('paused');
    };
    node.addEventListener('mouseenter', pause);
    node.addEventListener('mouseleave', resume);
    return {
      destroy() {
        clearTimeout(timer);
        node.removeEventListener('mouseenter', pause);
        node.removeEventListener('mouseleave', resume);
      },
    };
  }

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
</script>

<div class="stack" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div
      class="toast {toast.kind}"
      role={toast.kind === 'error' ? 'alert' : 'status'}
      style:--life="{lifetime(toast)}ms"
      use:autoDismiss={toast}
      in:fly={{ x: 32, duration: 320 * motion, opacity: 0 }}
      out:fly={{ x: 32, duration: 180 * motion, opacity: 0 }}
      animate:flip={{ duration: 260 * motion }}
    >
      <span class="badge">
        {#if toast.kind === 'capture'}
          <Logo size={24} />
        {:else}
          <Icon name={toast.kind === 'error' ? 'alert' : toast.kind === 'success' ? 'check' : 'info'} size={16} stroke={2} />
        {/if}
      </span>
      <div class="text">
        <strong>{toast.title}</strong>
        {#if toast.body}<span>{toast.body}</span>{/if}
      </div>
      {#if toast.action}
        <button class="action" onclick={() => (toast.action?.run(), dismissToast(toast.id))}>{toast.action.label}</button>
      {/if}
      <button class="close" onclick={() => dismissToast(toast.id)} aria-label="×"><Icon name="x" size={14} /></button>
      {#if toast.kind === 'capture'}<span class="timer"></span>{/if}
    </div>
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    right: 20px;
    bottom: 20px;
    z-index: 60;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 10px;
    pointer-events: none;
  }
  .toast {
    position: relative;
    display: flex;
    align-items: center;
    gap: 12px;
    width: 360px;
    padding: 12px 10px 12px 12px;
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--surface-2) 94%, transparent);
    backdrop-filter: blur(16px);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
    pointer-events: auto;
  }
  .badge {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    flex: none;
    border-radius: 9px;
    background: rgba(255, 255, 255, 0.06);
    color: var(--text-2);
  }
  .success .badge {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }
  .error .badge {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 14%, transparent);
  }
  .capture .badge {
    background: none;
    animation: pop 500ms var(--ease-out);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
    line-height: 1.35;
  }
  strong {
    font-size: 13.5px;
    font-weight: 600;
  }
  .text span {
    font-size: 12.5px;
    color: var(--text-2);
  }
  .action {
    height: 30px;
    padding: 0 12px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    background: var(--accent);
    color: var(--accent-fg);
    transition: filter var(--fast);
  }
  .action:hover {
    filter: brightness(1.1);
  }
  .close {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--text-3);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .close:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .timer {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    background: var(--accent);
    transform-origin: left;
    animation: drain var(--life) linear forwards;
  }
  :global(.toast.paused) .timer {
    animation-play-state: paused;
  }
  @keyframes drain {
    to {
      transform: scaleX(0);
    }
  }
  @keyframes pop {
    from {
      transform: scale(0.5) rotate(-18deg);
    }
  }
</style>
