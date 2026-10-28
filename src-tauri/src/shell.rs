//! Windows shell operations (Recycle Bin, Explorer, clipboard), run off the async runtime.

use std::path::PathBuf;

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Runs `f` on a brand-new OS thread and awaits its result (`None` if the thread could not run).
///
/// Shell APIs initialise COM on the calling thread and the `trash` crate aborts if the thread's
/// COM apartment was set up differently, so each call gets a clean thread. It also keeps these
/// blocking calls off the async runtime's workers.
pub async fn isolated<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    std::thread::Builder::new()
        .name("kryo-shell".into())
        .spawn(move || {
            let _ = tx.blocking_send(f());
        })
        .ok()?;
    rx.recv().await
}

/// Opens a folder in Explorer, creating it first if needed.
pub async fn open_dir(app: &AppHandle, dir: PathBuf) -> Result<(), &'static str> {
    const FAILED: &str = "open_failed";
    std::fs::create_dir_all(&dir).map_err(|_| FAILED)?;
    let app = app.clone();
    let dir = dir.to_string_lossy().into_owned();
    isolated(move || app.opener().open_path(dir, None::<&str>))
        .await
        .ok_or(FAILED)?
        .map_err(|_| FAILED)
}
