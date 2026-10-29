// The editor remembers each clip's trim and volumes, so reopening it starts where you left off.
// Kept per PC in local storage, keyed by shard id (moved on rename, dropped on delete).

export interface Edit {
  start: number;
  end: number;
  gains: { game: number; mic: number };
}

const KEY = 'krak.edits';

function readAll(): Record<string, Edit> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? '{}');
  } catch {
    return {};
  }
}

function writeAll(all: Record<string, Edit>) {
  try {
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // no storage: edits just aren't remembered
  }
}

export function getEdit(id: string): Edit | null {
  return readAll()[id] ?? null;
}

/** Saves the edit, or forgets it when it changes nothing. */
export function saveEdit(id: string, edit: Edit, duration: number) {
  const all = readAll();
  const untouched =
    edit.start <= 0.05 && edit.end >= duration - 0.05 && edit.gains.game === 1 && edit.gains.mic === 1;
  if (untouched) delete all[id];
  else all[id] = edit;
  writeAll(all);
}

export function moveEdit(from: string, to: string) {
  const all = readAll();
  if (!all[from]) return;
  all[to] = all[from];
  delete all[from];
  writeAll(all);
}

export function dropEdits(ids: string[]) {
  const all = readAll();
  let changed = false;
  for (const id of ids) {
    if (all[id]) {
      delete all[id];
      changed = true;
    }
  }
  if (changed) writeAll(all);
}
