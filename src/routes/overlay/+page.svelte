<!-- The in-game notification. Its window is transparent, click-through, never focused and
     excluded from screen capture; the backend shows it and hides it again after ~3.4 s. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Logo from '$lib/components/Logo.svelte';
  import { native, on } from '$lib/api';
  import { formatDuration, hasKey, t } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';

  interface Note {
    id: number;
    failed: boolean;
    title: string;
    body: string;
  }

  let note = $state<Note | null>(null);
  let shown = $state(false);
  let count = 0;
  let timer: ReturnType<typeof setTimeout>;

  async function present(failed: boolean, title: () => string, body: () => string) {
    // This window lives on its own: pick up a language change made in the main window.
    await settings.load();
    note = { id: ++count, failed, title: title(), body: body() };
    shown = true;
    clearTimeout(timer);
    timer = setTimeout(() => (shown = false), 2900);
  }

  onMount(() => {
    document.documentElement.classList.add('overlay');
    const listeners = [
      on('shard-saved', (shard) =>
        present(
          false,
          () => t('toast.captured'),
          () => `${shard.game ?? t('game.desktop')} · ${t('overlay.lastSeconds', { seconds: Math.round(shard.duration) })}`,
        ),
      ),
      on('screenshot-saved', (shot) =>
        present(
          false,
          () => t('screens.saved'),
          () => shot.game ?? t('game.desktop'),
        ),
      ),
      on('recording-saved', (shard) =>
        present(
          false,
          () => t('record.saved'),
          () => `${shard.game ?? t('game.desktop')} · ${formatDuration(shard.duration)}`,
        ),
      ),
      on('capture-failed', (code) => {
        const key = `capture.failed.${code}`;
        present(true, () => t(hasKey(key) ? key : 'capture.failed.encode_failed'), () => '');
      }),
    ];
    // Browser preview of /overlay: show one.
    if (!native) present(false, () => t('toast.captured'), () => `Rocket League · ${t('overlay.lastSeconds', { seconds: 30 })}`);
    return () => listeners.forEach((l) => l.then((unlisten) => unlisten()));
  });
</script>

<div class="wrap">
  {#if note}
    <div class="card" class:shown class:failed={note.failed}>
      {#key note.id}
        <span class="badge">
          {#if note.failed}<Icon name="alert" size={18} stroke={2} />{:else}<Logo size={26} color="var(--nuit)" />{/if}
        </span>
      {/key}
      <div class="text">
        <strong>{note.title}</strong>
        {#if note.body}<span>{note.body}</span>{/if}
      </div>
      {#key note.id}<span class="timer"></span>{/key}
    </div>
  {/if}
</div>

<style>
  :global(html.overlay),
  :global(html.overlay body) {
    background: transparent !important;
  }

  .wrap {
    position: fixed;
    inset: 0;
    display: flex;
    justify-content: flex-end;
    align-items: flex-start;
    padding: 14px 14px 0 0;
  }

  .card {
    position: relative;
    display: flex;
    align-items: center;
    gap: 14px;
    width: 330px;
    padding: 14px 22px 14px 14px;
    border-radius: 14px;
    background: rgba(10, 14, 20, 0.96);
    box-shadow:
      inset 0 0 0 1px #1b2430,
      0 14px 34px -10px rgba(0, 0, 0, 0.75);
    overflow: hidden;
    opacity: 0;
    transform: translateX(24px) scale(0.98);
    transition:
      opacity 220ms var(--ease-out),
      transform 420ms var(--ease-out);
  }
  .card.shown {
    opacity: 1;
    transform: none;
  }

  .badge {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 10px;
    background: var(--face);
    animation: pop 520ms var(--ease-out);
  }
  .failed .badge {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 16%, transparent);
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    line-height: 1.3;
  }
  strong {
    font-size: 16px;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: var(--text);
  }
  .text span {
    font-size: 13px;
    color: var(--text-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .timer {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    background: var(--accent);
    transform-origin: left;
    animation: drain 2.9s linear forwards;
  }
  .failed .timer {
    background: var(--danger);
  }

  @keyframes pop {
    from {
      transform: scale(0.5) rotate(-16deg);
      opacity: 0;
    }
  }
  @keyframes drain {
    to {
      transform: scaleX(0);
    }
  }
</style>
