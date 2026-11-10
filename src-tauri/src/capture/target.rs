//! Which window "game only" capture films.
//!
//! - Automatic: the game in front. A window counts as a game when it covers its whole monitor
//!   without being a maximized window (fullscreen or borderless), or when its program lives in a
//!   game library (Steam, Epic, Riot, FiveM...); browsers, chats, players, launchers and Windows'
//!   own windows never do. Once found it is kept while it exists, even behind other windows
//!   (alt-tab to Discord: the clip still shows the game), until another game comes in front.
//! - Picked: the app chosen in the list of open apps, found again by its program's file name or,
//!   failing that, its display name (games update their executables, FiveM names its process
//!   after the game build).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetClientRect, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use super::game;

/// Programs (lowercase file stems) that are never the game: browsers, chats, media players,
/// streaming and editing tools, launchers and stores, overlays.
const NOT_GAMES: &[&str] = &[
    "chrome",
    "msedge",
    "firefox",
    "opera",
    "opera_gx",
    "brave",
    "vivaldi",
    "arc",
    "iexplore",
    "discord",
    "discordptb",
    "discordcanary",
    "slack",
    "teams",
    "ms-teams",
    "zoom",
    "telegram",
    "whatsapp",
    "signal",
    "skype",
    "spotify",
    "vlc",
    "mpc-hc",
    "mpc-hc64",
    "mpc-be64",
    "potplayermini64",
    "potplayermini",
    "wmplayer",
    "music.ui",
    "video.ui",
    "photos",
    "obs64",
    "obs32",
    "streamlabs obs",
    "medal",
    "outplayed",
    "code",
    "devenv",
    "notepad",
    "notepad++",
    "winword",
    "excel",
    "powerpnt",
    "taskmgr",
    "cmd",
    "powershell",
    "pwsh",
    "windowsterminal",
    "conhost",
    "steam",
    "steamwebhelper",
    "epicgameslauncher",
    "battle.net",
    "riotclientux",
    "riotclientservices",
    "eadesktop",
    "origin",
    "upc",
    "galaxyclient",
    "gamebar",
    "nvidia overlay",
    "xboxpcapp",
];

/// Folders games are installed in (lowercase, with separators): a program under one is a game
/// even in a window.
const GAME_LIBRARIES: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\riot games\\",
    "\\fivem.app\\",
    "\\fivem\\",
    "\\ragemp\\",
    "\\altv\\",
    "\\ubisoft game launcher\\games\\",
    "\\ea games\\",
    "\\gog galaxy\\games\\",
    "\\xboxgames\\",
    "\\battle.net\\games\\",
];

/// Windows smaller than this (client area, pixels) are never filmed: splash screens, tool
/// windows.
const MIN_SIDE: i32 = 160;

/// The app the user picked (settings `captureGame`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPick {
    /// Program file name without extension, lowercase ("fivem_b3095_gtaprocess").
    pub exe: String,
    /// Display name ("FiveM"), to find the app again when its file name changes.
    pub name: String,
}

/// The window being filmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub hwnd: HWND,
    pub pid: u32,
    pub exe: PathBuf,
    /// Display name, `None` when its program has none worth showing.
    pub name: Option<String>,
}

// SAFETY: a window handle is a plain identifier, usable from any thread.
unsafe impl Send for Target {}
unsafe impl Sync for Target {}

/// An app with a window open, for the picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApp {
    pub exe: String,
    pub name: String,
    /// Its main window's title.
    pub title: String,
    /// Krak would pick it by itself (`is_game`).
    pub game: bool,
}

/// What a window looks like, for the decisions below.
#[derive(Debug, Clone)]
struct Window {
    hwnd: HWND,
    pid: u32,
    exe: PathBuf,
    title: String,
    /// The window's rectangle equals its monitor's (fullscreen or borderless).
    covers_monitor: bool,
    maximized: bool,
}

/// The window to film now (`None`: none, nothing is filmed).
pub fn find(pick: Option<&AppPick>, current: Option<&Target>) -> Option<Target> {
    let alive = current.filter(|t| alive(t)).cloned();
    match pick {
        None => {
            // SAFETY: plain query.
            let front = describe(unsafe { GetForegroundWindow() });
            match front {
                Some(front)
                    if is_game(&front) && alive.as_ref().is_none_or(|t| t.hwnd != front.hwnd) =>
                {
                    Some(target(front))
                }
                _ => alive,
            }
        }
        Some(pick) => {
            if alive.is_some() {
                return alive;
            }
            windows()
                .into_iter()
                .filter(|w| matches_pick(&w.exe, pick))
                .max_by_key(|w| area(w.hwnd))
                .map(target)
        }
    }
}

/// The apps with a window open, games first, each once (its largest window).
pub fn open_apps() -> Vec<OpenApp> {
    let mut apps: Vec<(OpenApp, i64)> = Vec::new();
    for window in windows() {
        let Some(exe) = stem(&window.exe) else {
            continue;
        };
        if game::DESKTOP_PROCESSES.contains(&exe.as_str()) {
            continue;
        }
        let size = area(window.hwnd);
        let game = is_game(&window);
        if let Some((app, best)) = apps.iter_mut().find(|(app, _)| app.exe == exe) {
            app.game |= game;
            if size > *best {
                app.title.clone_from(&window.title);
                *best = size;
            }
            continue;
        }
        let name = game::name_of(&window.exe).unwrap_or_else(|| exe.clone());
        apps.push((
            OpenApp {
                exe,
                name,
                title: window.title,
                game,
            },
            size,
        ));
    }
    let mut apps: Vec<OpenApp> = apps.into_iter().map(|(app, _)| app).collect();
    apps.sort_by(|a, b| {
        b.game
            .cmp(&a.game)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps
}

/// The window still exists and still belongs to the same process.
pub fn alive(target: &Target) -> bool {
    // SAFETY: plain queries on a handle that may be stale (they fail then).
    unsafe {
        let mut pid = 0u32;
        IsWindow(Some(target.hwnd)).as_bool()
            && GetWindowThreadProcessId(target.hwnd, Some(&mut pid)) != 0
            && pid == target.pid
    }
}

/// The window covers its whole monitor without being maximized: fullscreen or borderless.
pub fn fullscreen(target: &Target) -> bool {
    describe(target.hwnd).is_some_and(|w| w.covers_monitor && !w.maximized)
}

/// Where the window's content (its client area, without title bar and borders) lies in its
/// captured image, which covers the window's visible frame; `None` when it is the whole image or
/// cannot be told. Pixels, even (NV12).
pub fn content(target: &Target, image: (u32, u32)) -> Option<RECT> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    let mut frame = RECT::default();
    let mut client = RECT::default();
    let mut origin = POINT::default();
    // SAFETY: plain queries on the window; the out values are sized for each call.
    unsafe {
        DwmGetWindowAttribute(
            target.hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            std::ptr::from_mut(&mut frame).cast(),
            u32::try_from(std::mem::size_of::<RECT>()).unwrap_or(0),
        )
        .ok()?;
        GetClientRect(target.hwnd, &mut client).ok()?;
        ClientToScreen(target.hwnd, &mut origin).ok().ok()?;
    }
    crop(frame, origin, client, image)
}

/// The client area (`client` at screen `origin`) in an `image` of the window's `frame`.
fn crop(
    frame: RECT,
    origin: windows::Win32::Foundation::POINT,
    client: RECT,
    image: (u32, u32),
) -> Option<RECT> {
    let even = |v: i32| v.max(0) & !1;
    let (width, height) = (i32::try_from(image.0).ok()?, i32::try_from(image.1).ok()?);
    let left = even(origin.x - frame.left);
    let top = even(origin.y - frame.top);
    let right = (left + even(client.right - client.left)).min(width & !1);
    let bottom = (top + even(client.bottom - client.top)).min(height & !1);
    let rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    let whole = left == 0 && top == 0 && right >= (width & !1) && bottom >= (height & !1);
    (right - left >= MIN_SIDE && bottom - top >= MIN_SIDE && !whole).then_some(rect)
}

fn target(window: Window) -> Target {
    Target {
        name: game::name_of(&window.exe),
        hwnd: window.hwnd,
        pid: window.pid,
        exe: window.exe,
    }
}

fn is_game(window: &Window) -> bool {
    let Some(exe) = stem(&window.exe) else {
        return false;
    };
    if NOT_GAMES.contains(&exe.as_str()) || game::DESKTOP_PROCESSES.contains(&exe.as_str()) {
        return false;
    }
    (window.covers_monitor && !window.maximized) || in_game_library(&window.exe)
}

fn in_game_library(exe: &Path) -> bool {
    let path = exe.to_string_lossy().to_lowercase();
    GAME_LIBRARIES.iter().any(|folder| path.contains(folder))
}

fn matches_pick(exe: &Path, pick: &AppPick) -> bool {
    if stem(exe).is_some_and(|s| s == pick.exe) {
        return true;
    }
    !pick.name.is_empty()
        && game::name_of(exe).is_some_and(|name| name.eq_ignore_ascii_case(&pick.name))
}

/// Lowercase file name without extension.
fn stem(exe: &Path) -> Option<String> {
    exe.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase)
}

/// Client area in pixels (0 for a minimized window).
fn area(hwnd: HWND) -> i64 {
    let mut rect = RECT::default();
    // SAFETY: plain query.
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return 0;
    }
    i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
}

/// The visible main windows of other processes.
fn windows() -> Vec<Window> {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        // SAFETY: `found` is the Vec passed to EnumWindows below, alive for the whole call.
        let found = unsafe { &mut *(found.0 as *mut Vec<HWND>) };
        found.push(hwnd);
        BOOL(1)
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    let _ = unsafe {
        EnumWindows(
            Some(each),
            LPARAM(std::ptr::from_mut(&mut handles) as isize),
        )
    };
    handles.into_iter().filter_map(describe).collect()
}

/// A main window worth filming, described; `None` for anything else (hidden, cloaked, owned,
/// tool windows, tiny ones, ours).
fn describe(hwnd: HWND) -> Option<Window> {
    // SAFETY: plain queries on a window handle; every buffer is sized for its call.
    unsafe {
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        if GetAncestor(hwnd, GA_ROOT) != hwnd
            || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid())
        {
            return None;
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            4,
        )
        .is_ok()
            && cloaked != 0
        {
            return None;
        }
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client).ok()?;
        if client.right - client.left < MIN_SIDE || client.bottom - client.top < MIN_SIDE {
            return None;
        }
        let (exe, pid) = game::window_process(hwnd)?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or(0),
            ..Default::default()
        };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let covers_monitor =
            GetMonitorInfoW(monitor, &mut info).as_bool() && rect == info.rcMonitor;
        let length = GetWindowTextLengthW(hwnd).max(0);
        let mut title = vec![0u16; usize::try_from(length).unwrap_or(0) + 1];
        let copied = GetWindowTextW(hwnd, &mut title);
        title.truncate(usize::try_from(copied).unwrap_or(0));
        Some(Window {
            hwnd,
            pid,
            exe,
            title: String::from_utf16_lossy(&title),
            covers_monitor,
            maximized: IsZoomed(hwnd).as_bool(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(exe: &str, covers_monitor: bool, maximized: bool) -> Window {
        Window {
            hwnd: HWND::default(),
            pid: 1,
            exe: PathBuf::from(exe),
            title: String::new(),
            covers_monitor,
            maximized,
        }
    }

    #[test]
    fn tells_games_from_other_apps() {
        // Fullscreen or borderless: a game.
        assert!(is_game(&window(
            r"D:\Games\Valorant\VALORANT-Win64-Shipping.exe",
            true,
            false
        )));
        // A game library, even in a window.
        let fivem = r"C:\Users\me\AppData\Local\FiveM\FiveM.app\data\FiveM_b3095_GTAProcess.exe";
        assert!(is_game(&window(fivem, false, false)));
        assert!(is_game(&window(
            r"C:\Program Files (x86)\Steam\steamapps\common\Celeste\Celeste.exe",
            false,
            false
        )));
        // A maximized app covers its monitor too, but is not a game.
        assert!(!is_game(&window(r"C:\Tools\Paint.exe", true, true)));
        // A plain window outside a library: not picked by itself.
        assert!(!is_game(&window(r"C:\Tools\Paint.exe", false, false)));
        // Browsers, chats and launchers never, even fullscreen.
        for exe in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Users\me\AppData\Local\Discord\app-1.0\Discord.exe",
            r"C:\Program Files (x86)\Steam\steam.exe",
            r"C:\Windows\explorer.exe",
        ] {
            assert!(!is_game(&window(exe, true, false)), "{exe}");
        }
    }

    #[test]
    fn crops_the_title_bar_and_borders() {
        use windows::Win32::Foundation::POINT;
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        // A 960x540 window at (100, 50): a 31 px title bar, 1 px borders.
        let frame = rect(100, 50, 1060, 590);
        let client = rect(0, 0, 958, 508);
        let cut = crop(frame, POINT { x: 101, y: 81 }, client, (960, 540)).expect("cropped");
        assert_eq!(
            (cut.left, cut.top, cut.right, cut.bottom),
            (0, 30, 958, 538)
        );
        // Borderless (the client is the whole window): nothing to crop.
        let full = rect(0, 0, 1920, 1080);
        assert!(crop(full, POINT { x: 0, y: 0 }, full, (1920, 1080)).is_none());
    }

    #[test]
    fn finds_a_picked_app_by_its_file_name() {
        let pick = AppPick {
            exe: "fivem_b3095_gtaprocess".into(),
            name: "FiveM".into(),
        };
        assert!(matches_pick(
            Path::new(r"C:\x\FiveM_b3095_GTAProcess.exe"),
            &pick
        ));
        assert!(!matches_pick(Path::new(r"C:\x\notepad.exe"), &pick));
    }

    #[test]
    fn open_windows_never_include_krak() {
        let own = std::env::current_exe().expect("exe");
        assert!(windows().iter().all(|w| w.exe != own));
        assert!(open_apps().iter().all(|app| !app.exe.is_empty()));
    }
}
