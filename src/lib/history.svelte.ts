// Undo and redo (Ctrl+Z, Ctrl+Y) over snapshots of some state: the editor's edit of a clip, the
// best-of. The owner calls `changed()` on every change; a step is recorded once changes settle,
// so a drag, a volume slide or a burst of typing is undone in one go (nothing is recorded while
// a pointer is held down). Restoring a snapshot is not recorded.

/** Changes this close together make a single step (ms). */
const SETTLE = 400;
/** Steps kept. */
const LIMIT = 100;

export class History<T> {
  #take: () => T;
  #restore: (state: T) => void;
  #past: string[] = [];
  #future: string[] = [];
  /** The state as last recorded; null until `reset`. */
  #committed: string | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #held = false;

  canUndo = $state(false);
  canRedo = $state(false);

  /** `take` reads the state (a plain copy), `restore` puts a copy back. */
  constructor(take: () => T, restore: (state: T) => void) {
    this.#take = take;
    this.#restore = restore;
  }

  /** Starts over from the current state (another clip): nothing to undo or redo. */
  reset() {
    this.#cancel();
    this.#past = [];
    this.#future = [];
    this.#committed = JSON.stringify(this.#take());
    this.#sync();
  }

  /** Something changed: recorded as a step once changes settle. */
  changed() {
    if (this.#committed === null) return;
    this.#cancel();
    this.#timer = setTimeout(() => this.#commit(), SETTLE);
  }

  /** A pointer went down (true) or up (false): a drag is recorded when it ends. */
  hold(down: boolean) {
    this.#held = down;
    if (!down && this.#committed !== null) this.changed();
  }

  undo(): boolean {
    this.#commit(true);
    const previous = this.#past.pop();
    if (previous === undefined || this.#committed === null) return false;
    this.#future.push(this.#committed);
    this.#apply(previous);
    return true;
  }

  redo(): boolean {
    this.#commit(true);
    const next = this.#future.pop();
    if (next === undefined || this.#committed === null) return false;
    this.#past.push(this.#committed);
    this.#apply(next);
    return true;
  }

  /** Stops recording (the owner is going away). */
  dispose() {
    this.#cancel();
    this.#committed = null;
  }

  #apply(state: string) {
    this.#committed = state;
    this.#restore(JSON.parse(state));
    this.#sync();
  }

  #commit(force = false) {
    this.#cancel();
    if (this.#committed === null || (this.#held && !force)) return;
    const now = JSON.stringify(this.#take());
    if (now === this.#committed) return;
    this.#past.push(this.#committed);
    if (this.#past.length > LIMIT) this.#past.shift();
    this.#future = [];
    this.#committed = now;
    this.#sync();
  }

  #cancel() {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
  }

  #sync() {
    this.canUndo = this.#past.length > 0;
    this.canRedo = this.#future.length > 0;
  }
}
