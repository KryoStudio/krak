// The one-time invitation to the Krak Discord: once the app has been used (a second launch, or a
// first shard), never during the first-run wizard. "Not now" asks once more a week later; the
// answer is kept in the settings, so it survives a cleared WebView.
import type { Settings } from './api';
import { discordInvite, openLink } from './community';
import { library } from './library.svelte';
import { settings } from './settings.svelte';

const WEEK = 7 * 24 * 60 * 60 * 1000;
/** Into the session before it may show: never at the same instant as everything else. */
const SETTLE_MS = 6000;

/** Whether the invitation is due at `now` (ms), the app having been `used` already. */
export function inviteDue(state: Settings['communityInvite'], at: number | null, now: number, used: boolean): boolean {
  if (state === 'pending') return used;
  if (state === 'later') return at === null || now >= at + WEEK;
  return false;
}

class Invite {
  /** Krak was already set up when this session began: at least its second launch. */
  #returning = $state(false);
  #settled = $state(false);
  #now = $state(Date.now());

  readonly due = $derived(
    this.#settled &&
      settings.value.onboarded &&
      inviteDue(settings.value.communityInvite, settings.value.communityInviteAt, this.#now, this.#returning || library.shards.length > 0),
  );

  /** Starts watching (main window layout); returns the cleanup. */
  start(): () => void {
    this.#returning = settings.value.onboarded;
    const settle = setTimeout(() => (this.#settled = true), SETTLE_MS);
    // A Krak that lives in the tray for days still asks again when the week is over.
    const clock = setInterval(() => (this.#now = Date.now()), 30 * 60 * 1000);
    return () => {
      clearTimeout(settle);
      clearInterval(clock);
    };
  }

  join() {
    openLink(discordInvite('app-welcome'));
    this.#answer({ communityInvite: 'joined' });
  }

  /** Asked once more in a week; the second time, not again. */
  notNow() {
    if (settings.value.communityInvite === 'later') this.#answer({ communityInvite: 'never' });
    else this.#answer({ communityInvite: 'later', communityInviteAt: Date.now() });
  }

  never() {
    this.#answer({ communityInvite: 'never' });
  }

  #answer(patch: Partial<Settings>) {
    settings.update(patch).catch(() => {});
  }
}

export const invite = new Invite();
