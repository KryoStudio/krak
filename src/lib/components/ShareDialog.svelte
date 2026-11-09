<!-- "Share to the Krak community": what leaves the PC (asked once), linking Krak to the user's
     Discord (a code confirmed in a DM from the bot or with a command), the post's title, game
     and spoiler, then the export (sized for the server) and the upload, and the post. Nothing
     reaches the network before this sheet is opened. -->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { fade, fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import Select from './Select.svelte';
  import Toggle from './Toggle.svelte';
  import { api, communityError, native, on, type CommunityError, type CommunityMe, type CommunityUser, type Pairing } from '$lib/api';
  import { CLIPS_CHANNEL, discordInvite, openLink } from '$lib/community';
  import { hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { portal } from '$lib/portal';
  import { settings } from '$lib/settings.svelte';
  import { consented, expiryTime, giveConsent, share, type ShareRequest } from '$lib/share.svelte';
  import { displayTitle } from '$lib/shard-actions';
  import { toast } from '$lib/ui.svelte';

  let { request }: { request: ShareRequest } = $props();
  const shard = $derived(request.shard);

  type Stage = 'consent' | 'connecting' | 'soon' | 'pairing' | 'refused' | 'form' | 'working' | 'done' | 'error';
  let stage = $state<Stage>(consented() ? 'connecting' : 'consent');
  let pairing = $state<Pairing | null>(null);
  let pairState = $state<'waiting' | 'expired' | 'denied'>('waiting');
  let now = $state(Date.now());
  let me = $state<CommunityMe | null>(null);
  let failure = $state<CommunityError | null>(null);
  let title = $state('');
  let game = $state<string | null>(null);
  let spoiler = $state(false);
  let progress = $state(0);
  let phase = $state<'export' | 'upload'>('export');
  let posted = $state<{ threadId: string; url: string } | null>(null);

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  let dialog: HTMLDivElement;
  let alive = true;
  let poll: ReturnType<typeof setTimeout> | undefined;
  let clock: ReturnType<typeof setInterval> | undefined;

  onMount(() => {
    title = displayTitle(request.shard).slice(0, 100);
    dialog.focus();
    if (stage === 'connecting') start();
    clock = setInterval(() => (now = Date.now()), 1000);
  });
  onDestroy(() => {
    alive = false;
    clearTimeout(poll);
    clearInterval(clock);
  });

  function close() {
    if (stage === 'working') return;
    share.request = null;
  }

  function agree() {
    giveConsent();
    start();
  }

  async function start() {
    if (!native || library.demo) {
      stage = 'soon';
      return;
    }
    stage = 'connecting';
    try {
      const info = await api.communityInfo();
      if (!alive) return;
      if (!info.api) {
        stage = 'soon';
        return;
      }
      if (await api.communityAccount()) await loadMe();
      else await pair();
    } catch (e) {
      fail(e);
    }
  }

  async function pair() {
    stage = 'pairing';
    pairing = null;
    pairState = 'waiting';
    try {
      pairing = await api.communityPair();
      if (alive) schedulePoll();
    } catch (e) {
      fail(e);
    }
  }

  function schedulePoll() {
    clearTimeout(poll);
    poll = setTimeout(checkPair, 2500);
  }

  async function checkPair() {
    if (!alive || stage !== 'pairing' || !pairing) return;
    try {
      const status = await api.communityPairStatus();
      if (!alive) return;
      if (status.status === 'pending') {
        const expires = expiryTime(pairing.expiresAt);
        if (expires && Date.now() > expires + 5000) pairState = 'expired';
        else schedulePoll();
      } else if (status.status === 'linked') await loadMe();
      else pairState = status.status;
    } catch (e) {
      const err = communityError(e);
      // A blip of the connection: keep waiting.
      if (err.code === 'community_offline') schedulePoll();
      else if (err.code === 'community_no_pairing') pairState = 'expired';
      else fail(e);
    }
  }

  async function loadMe() {
    stage = 'connecting';
    try {
      const found = await api.communityMe();
      if (!alive) return;
      me = found;
      if (!found.canShare || !found.shareLimit) {
        stage = 'refused';
        return;
      }
      game = pickGame(found.games);
      stage = 'form';
    } catch (e) {
      if (communityError(e).code === 'community_unlinked') await pair();
      else fail(e);
    }
  }

  /** The shard's game, as the bot spells it when it knows it. */
  function pickGame(games: string[]): string | null {
    const own = shard.game;
    if (!own) return null;
    return games.find((g) => g.toLowerCase() === own.toLowerCase()) ?? own;
  }

  const gameOptions = $derived.by(() => {
    const names = [...(me?.games ?? [])];
    const own = shard.game;
    if (own && !names.some((g) => g.toLowerCase() === own.toLowerCase())) names.unshift(own);
    return [{ value: null as string | null, label: t('share.noGame') }, ...names.map((n) => ({ value: n as string | null, label: n }))];
  });

  async function post() {
    const limit = me?.shareLimit;
    if (!limit || !title.trim()) return;
    stage = 'working';
    phase = 'export';
    progress = 0;
    const stopExport = on('export-progress', (p) => {
      if (p.id === shard.id && phase === 'export') progress = p.progress * 0.5;
    });
    const stopUpload = on('community-upload-progress', (p) => {
      if (phase === 'upload' && p.total) progress = 0.5 + 0.5 * (p.sent / p.total);
    });
    try {
      const exported = await api.exportShard({
        id: shard.id,
        start: request.start,
        end: request.end,
        gains: { ...request.gains },
        target: 'share',
        maxBytes: limit,
        watermark: settings.value.watermark,
        vertical: request.vertical,
        cropX: request.cropX,
      });
      if (!alive) return;
      phase = 'upload';
      progress = 0.5;
      posted = await api.communityUpload(exported.path, title.trim(), game, spoiler);
      progress = 1;
      stage = 'done';
    } catch (e) {
      const err = communityError(e);
      if (err.code === 'cancelled') stage = 'form';
      else if (err.code === 'community_unlinked') await pair();
      else fail(e);
    } finally {
      stopExport.then((unlisten) => unlisten());
      stopUpload.then((unlisten) => unlisten());
    }
  }

  function cancel() {
    if (phase === 'export') api.cancelExport().catch(() => {});
    else api.communityCancelUpload().catch(() => {});
  }

  function fail(e: unknown) {
    if (!alive) return;
    failure = communityError(e);
    stage = 'error';
  }

  function retry() {
    if (me?.shareLimit) stage = 'form';
    else start();
  }

  const failureText = $derived.by(() => {
    if (!failure) return '';
    const key = `error.${failure.code}`;
    const text = t(hasKey(key) ? key : 'error.community_failed');
    if (failure.code === 'community_rate_limited' && failure.retryAfter) {
      return `${text} ${t('share.retryIn', { minutes: Math.max(1, Math.ceil(failure.retryAfter / 60)) })}`;
    }
    return text;
  });

  async function removePost() {
    if (!posted) return;
    try {
      await api.communityDelete(posted.threadId);
      toast({ kind: 'info', title: t('share.deleted') });
      share.request = null;
    } catch (e) {
      fail(e);
    }
  }

  async function unlink() {
    await api.communityUnlink().catch(() => {});
    me = null;
    await pair();
  }

  function copyCommand() {
    navigator.clipboard
      .writeText(command)
      .then(() => toast({ kind: 'success', title: t('share.copied') }))
      .catch(() => {});
  }

  const expiresIn = $derived.by(() => {
    const at = pairing ? expiryTime(pairing.expiresAt) : null;
    if (!at) return null;
    const seconds = Math.max(0, Math.round((at - now) / 1000));
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
  });
  const command = $derived(pairing ? `/share link code:${pairing.code}` : '');
  const nameOf = (user: CommunityUser | null | undefined) => user?.globalName || user?.username || '';

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') {
      if (stage === 'working') cancel();
      else close();
    } else if (e.key === 'Enter' && stage === 'form' && (e.target as HTMLElement).tagName !== 'BUTTON') post();
  }
</script>

<div class="backdrop" use:portal={'body'} transition:fade={{ duration: 180 * motion }} onpointerdown={close} role="presentation"></div>
<div class="center" use:portal={'body'}>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div bind:this={dialog} class="dialog" role="dialog" aria-modal="true" aria-label={t('share.title')} tabindex="-1" {onkeydown} in:scale={{ start: 0.96, duration: 260 * motion, easing: cubicOut }} out:scale={{ start: 0.98, duration: 140 * motion }}>
    <header>
      <div>
        <h2>{t('share.title')}</h2>
        <p>{displayTitle(shard)}</p>
      </div>
      <button class="close" onclick={close} aria-label={t('common.close')} disabled={stage === 'working'}><Icon name="x" size={16} /></button>
    </header>

    {#key stage}
      <div class="stage" in:fly={{ y: 8, duration: 220 * motion }}>
        {#if stage === 'consent'}
          <div class="hero"><Icon name="users" size={26} /></div>
          <h3>{t('share.consentTitle')}</h3>
          <p>{t('share.consentBody')}</p>
          <ul class="sent">
            <li><Icon name="film" size={15} />{t('share.consentClip')}</li>
            <li><Icon name="pencil" size={15} />{t('share.consentDetails')}</li>
            <li><Icon name="users" size={15} />{t('share.consentAccount')}</li>
          </ul>
          <p class="small">{t('share.consentWhere')}</p>
          <footer>
            <button class="secondary" onclick={close}>{t('common.cancel')}</button>
            <button class="primary" onclick={agree}>{t('share.consentAgree')}</button>
          </footer>
        {:else if stage === 'connecting'}
          <div class="waiting big"><span class="spin"><Icon name="spinner" size={22} /></span>{t('share.connecting')}</div>
        {:else if stage === 'soon'}
          <div class="hero"><Icon name="clock" size={26} /></div>
          <h3>{t('share.soonTitle')}</h3>
          <p>{t('share.soonBody')}</p>
          <footer>
            <button class="secondary" onclick={() => openLink(discordInvite('app-share'))}><Icon name="users" size={14} />{t('community.join')}</button>
            <button class="primary" onclick={() => openLink(CLIPS_CHANNEL)}><Icon name="message" size={14} />{t('export.postClips')}</button>
          </footer>
        {:else if stage === 'pairing'}
          <h3>{t('share.linkTitle')}</h3>
          {#if !pairing}
            <div class="waiting big"><span class="spin"><Icon name="spinner" size={22} /></span></div>
          {:else if pairState === 'waiting'}
            {#if pairing.via === 'dm'}
              <p>{t('share.linkDm')}</p>
              <div class="code" aria-label={pairing.code}>{pairing.code}</div>
            {:else}
              <p>{t('share.linkCommand')}</p>
              <div class="command">
                <code>{command}</code>
                <button class="secondary small-button" onclick={copyCommand}><Icon name="copy" size={13} />{t('share.copyCommand')}</button>
              </div>
            {/if}
            <div class="waiting">
              <span class="spin"><Icon name="spinner" size={14} /></span>{t('share.linkWaiting')}
              {#if expiresIn}<span class="dim">· {t('share.linkExpires', { time: expiresIn })}</span>{/if}
            </div>
            <footer>
              <button class="secondary" onclick={close}>{t('common.cancel')}</button>
              {#if pairing.via === 'dm'}
                <button class="primary" onclick={() => openLink('https://discord.com/channels/@me')}><Icon name="external" size={14} />{t('share.openDiscord')}</button>
              {:else}
                <button class="primary" onclick={() => openLink(discordInvite('app-share'))}><Icon name="external" size={14} />{t('share.openDiscord')}</button>
              {/if}
            </footer>
          {:else}
            <p class="warn">{t(pairState === 'expired' ? 'share.linkExpired' : 'share.linkDenied')}</p>
            <footer>
              <button class="secondary" onclick={close}>{t('common.close')}</button>
              <button class="primary" onclick={pair}><Icon name="undo" size={14} />{t('share.linkRetry')}</button>
            </footer>
          {/if}
        {:else if stage === 'refused'}
          <div class="hero warn-hero"><Icon name="alert" size={26} /></div>
          <h3>{t('share.cannot')}</h3>
          {#if me?.reason === 'not_member'}<p>{t('share.cannotMember')}</p>{/if}
          <footer>
            <button class="secondary" onclick={close}>{t('common.close')}</button>
            <button class="primary" onclick={() => openLink(discordInvite('app-share'))}><Icon name="users" size={14} />{t('community.join')}</button>
          </footer>
        {:else if stage === 'form' && me}
          <label class="field">
            <span>{t('share.titleLabel')}</span>
            <input bind:value={title} maxlength="100" spellcheck="false" />
          </label>
          <div class="field">
            <span>{t('share.gameLabel')}</span>
            <Select label={t('share.gameLabel')} options={gameOptions} value={game} onchange={(v) => (game = v)} width={300} />
          </div>
          <div class="row">
            <div class="row-text">
              <strong>{t('share.spoiler')}</strong>
              <span>{t('share.spoilerHint')}</span>
            </div>
            <Toggle label={t('share.spoiler')} checked={spoiler} onchange={(v) => (spoiler = v)} />
          </div>
          <p class="as">
            {#if me.user?.avatarUrl}<img src={me.user.avatarUrl} alt="" width="20" height="20" referrerpolicy="no-referrer" />{/if}
            <span>{t('share.as', { name: nameOf(me.user) })}</span>
            <button class="link" onclick={unlink}>{t('share.unlink')}</button>
            {#if me.remaining.day !== null}<span class="dim">· {t('share.remaining', { count: me.remaining.day })}</span>{/if}
          </p>
          <footer>
            <button class="secondary" onclick={close}>{t('common.cancel')}</button>
            <button class="primary" onclick={post} disabled={!title.trim() || me.remaining.day === 0}><Icon name="share" size={14} />{t('share.post')}</button>
          </footer>
        {:else if stage === 'working'}
          <div class="status">
            <div class="ring" style:--p={progress}><span>{Math.round(progress * 100)} %</span></div>
            <p>{phase === 'export' ? t('share.exporting', { percent: Math.round(progress * 200) }) : t('share.uploading', { percent: Math.round((progress - 0.5) * 200) })}</p>
            <button class="secondary" onclick={cancel}>{t('common.cancel')}</button>
          </div>
        {:else if stage === 'done'}
          <div class="status">
            <div class="done-icon"><Icon name="check" size={30} stroke={2.4} /></div>
            <h3>{t('share.doneTitle')}</h3>
            <p>{t('share.doneBody')}</p>
            <div class="done-actions">
              <button class="secondary" onclick={removePost}><Icon name="trash" size={14} />{t('share.delete')}</button>
              {#if posted?.url}
                <button class="secondary" onclick={() => posted && openLink(posted.url)}><Icon name="external" size={14} />{t('share.open')}</button>
              {/if}
              <button class="primary" onclick={close}>{t('export.done')}</button>
            </div>
          </div>
        {:else if stage === 'error'}
          <div class="hero warn-hero"><Icon name="alert" size={26} /></div>
          <p class="center-text">{failureText}</p>
          <footer>
            <button class="secondary" onclick={close}>{t('common.close')}</button>
            <button class="primary" onclick={retry}><Icon name="undo" size={14} />{t('share.retry')}</button>
          </footer>
        {/if}
      </div>
    {/key}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: rgba(5, 6, 8, 0.6);
    backdrop-filter: blur(4px);
  }
  .center {
    position: fixed;
    inset: 0;
    z-index: 51;
    display: grid;
    place-items: center;
    padding: 24px;
    pointer-events: none;
  }
  .dialog {
    display: flex;
    flex-direction: column;
    width: min(500px, 100%);
    max-height: calc(100vh - 48px);
    overflow-y: auto;
    padding: 20px 22px;
    border-radius: 18px;
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 40px 100px -30px rgba(0, 0, 0, 0.9);
    pointer-events: auto;
    outline: none;
  }
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 16px;
  }
  h2 {
    font-size: 18px;
    font-weight: 650;
  }
  header p {
    margin-top: 2px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  h3 {
    font-size: 16px;
    font-weight: 650;
  }
  .close {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--text-3);
  }
  .close:hover:not(:disabled) {
    color: var(--text);
    background: rgba(255, 255, 255, 0.06);
  }
  .stage {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .stage > p {
    font-size: 13.5px;
    line-height: 1.5;
    color: var(--text-2);
  }
  .stage > p.small {
    font-size: 12.5px;
    color: var(--text-3);
  }
  .hero {
    display: grid;
    place-items: center;
    width: 52px;
    height: 52px;
    border-radius: 14px;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .warn-hero {
    color: #f5a524;
    background: color-mix(in srgb, #f5a524 14%, transparent);
  }
  .sent {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-radius: 12px;
    list-style: none;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .sent li {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }
  .sent li :global(svg) {
    flex: none;
    color: var(--accent);
  }
  .code {
    align-self: center;
    padding: 10px 22px;
    border-radius: 12px;
    font-size: 30px;
    font-weight: 700;
    letter-spacing: 0.25em;
    font-variant-numeric: tabular-nums;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    user-select: all;
  }
  .command {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 10px 10px 14px;
    border-radius: 12px;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .command code {
    flex: 1;
    font-size: 14px;
    user-select: all;
  }
  .waiting {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  .waiting.big {
    justify-content: center;
    padding: 28px 0;
    font-size: 13.5px;
  }
  .spin {
    display: inline-grid;
    animation: spin 900ms linear infinite;
  }
  .dim {
    color: var(--text-3);
  }
  .warn {
    color: #f5a524 !important;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .field > span {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .field input {
    height: 38px;
    padding: 0 12px;
    border-radius: 10px;
    font-size: 14px;
    color: var(--text);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    outline: none;
  }
  .field input:focus {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .row-text {
    display: flex;
    flex-direction: column;
    flex: 1;
  }
  .row-text strong {
    font-size: 13px;
    font-weight: 600;
  }
  .row-text span {
    font-size: 12px;
    color: var(--text-3);
  }
  .as {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: 12.5px !important;
  }
  .as img {
    border-radius: 50%;
  }
  .link {
    font-size: 12.5px;
    color: var(--accent);
  }
  .link:hover {
    text-decoration: underline;
  }
  .center-text {
    text-align: center;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 6px;
  }
  .primary,
  .secondary {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    padding: 0 16px;
    border-radius: 10px;
    font-weight: 600;
    transition:
      filter var(--fast),
      background var(--fast),
      transform var(--fast) var(--ease-out);
  }
  .small-button {
    height: 30px;
    padding: 0 10px;
    font-size: 12.5px;
  }
  .primary {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .primary:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .primary:disabled {
    opacity: 0.5;
  }
  .secondary {
    background: rgba(255, 255, 255, 0.06);
  }
  .secondary:hover {
    background: rgba(255, 255, 255, 0.1);
  }
  .primary:active:not(:disabled),
  .secondary:active {
    transform: scale(0.97);
  }
  .status {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 12px 0 4px;
    text-align: center;
  }
  .status p {
    color: var(--text-2);
  }
  .ring {
    display: grid;
    place-items: center;
    width: 96px;
    height: 96px;
    border-radius: 50%;
    background: conic-gradient(var(--accent) calc(var(--p) * 1turn), var(--surface-3) 0);
    transition: background 200ms linear;
  }
  .ring span {
    display: grid;
    place-items: center;
    width: 80px;
    height: 80px;
    border-radius: 50%;
    background: var(--surface);
    font-size: 16px;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }
  .done-icon {
    display: grid;
    place-items: center;
    width: 72px;
    height: 72px;
    border-radius: 50%;
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }
  .done-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 6px;
  }

  @keyframes spin {
    to {
      transform: rotate(1turn);
    }
  }
</style>
