// The main window as the user has it. `shown`: on screen (not minimized, not hidden in the
// tray). `focused`: shown and in front, the app being used. Videos stop when Krak is put away,
// and the home preview only loops while Krak is in front, so a game on the other screen never
// shares the graphics card with it.
import { getCurrentWindow } from '@tauri-apps/api/window';
import { native } from './api';

export const appWindow = $state({ shown: true, focused: true });

let minimized = false;
let focused = typeof document === 'undefined' || document.hasFocus();

function sync() {
  appWindow.shown = document.visibilityState === 'visible' && !minimized;
  appWindow.focused = appWindow.shown && focused;
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', sync);
  window.addEventListener('focus', () => ((focused = true), sync()));
  window.addEventListener('blur', () => ((focused = false), sync()));
  if (native) {
    // A minimized window can stay "visible" to the page: ask the window itself.
    const win = getCurrentWindow();
    const check = () =>
      win.isMinimized().then(
        (value) => ((minimized = value), sync()),
        () => {},
      );
    win.onResized(check).catch(() => {});
    win.onFocusChanged(({ payload }) => ((focused = payload), sync())).catch(() => {});
    check();
  }
  sync();
}
