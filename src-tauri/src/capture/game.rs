//! Which game is being played: the foreground window's process, named after its version info
//! (FileDescription, else ProductName, else the exe name). Shell windows count as "no game".

use std::path::Path;

use crate::names;

/// Executables (lowercase stems) that are the desktop, not a game.
const DESKTOP_PROCESSES: [&str; 16] = [
    "explorer",
    "krak",
    "kryo-shard",
    "searchhost",
    "searchapp",
    "searchui",
    "startmenuexperiencehost",
    "shellexperiencehost",
    "shellhost",
    "lockapp",
    "logonui",
    "consent",
    "textinputhost",
    "applicationframehost",
    "dwm",
    "rundll32",
];

/// Version-info names that say nothing about the game (engine stubs and placeholders).
const GENERIC_NAMES: [&str; 10] = [
    "game",
    "launcher",
    "application",
    "bootstrappackagedgame",
    "unreal engine",
    "ue4 game",
    "ue5 game",
    "unityplayer",
    "unity player",
    "unity",
];

/// Unreal Engine executable suffixes, e.g. `FortniteClient-Win64-Shipping`.
const UNREAL_PLATFORMS: [&str; 3] = ["-win64", "-win32", "-wingdk"];
const UNREAL_CONFIGS: [&str; 4] = ["-shipping", "-test", "-development", "-debug"];

/// The foreground game's name, `None` for the desktop, shell or our own window.
#[cfg(windows)]
pub fn foreground_game() -> Option<String> {
    let exe = foreground_exe()?;
    game_name(&exe, &version_names(&exe))
}

/// Picks the display name of `exe` from its version-info names, or `None` for desktop
/// processes.
pub fn game_name(exe: &Path, version: &[String]) -> Option<String> {
    let stem = exe.file_stem()?.to_str()?;
    if DESKTOP_PROCESSES.contains(&stem.to_ascii_lowercase().as_str()) {
        return None;
    }
    version
        .iter()
        .filter_map(|name| clean_name(name))
        .find(|name| !GENERIC_NAMES.contains(&name.to_lowercase().as_str()))
        .or_else(|| clean_name(stem))
}

/// Removes trademark signs and Unreal suffixes, decodes the XML entities some version
/// resources contain, collapses whitespace and makes the result a valid folder name the library
/// will list.
pub fn clean_name(raw: &str) -> Option<String> {
    let decoded = raw
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let mut name: String = decoded
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©'))
        .collect();
    for mark in ["(tm)", "(r)", "(c)"] {
        while let Some(at) = name.to_ascii_lowercase().find(mark) {
            name.replace_range(at..at + mark.len(), " ");
        }
    }
    let mut name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(stripped) = strip_unreal_suffix(&name) {
        name = stripped;
    }
    // The library skips folders starting with '.' or '$'.
    let name = name.trim_start_matches(['.', '$']);
    names::sanitize_title(name)
}

fn strip_unreal_suffix(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    for config in UNREAL_CONFIGS {
        let Some(head) = lower.strip_suffix(config) else {
            continue;
        };
        for platform in UNREAL_PLATFORMS {
            if let Some(base) = head.strip_suffix(platform) {
                return Some(name[..base.len()].to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn foreground_exe() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    use windows::core::PWSTR;

    // SAFETY: plain queries; the process handle is closed before returning.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = u32::try_from(buf.len()).unwrap_or(0);
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        buf.truncate(len as usize);
        Some(std::ffi::OsString::from_wide(&buf).into())
    }
}

/// FileDescription then ProductName of `exe`, in the file's own language(s).
#[cfg(windows)]
fn version_names(exe: &Path) -> Vec<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::core::{HSTRING, PCWSTR};

    let wide: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: the version block is sized by GetFileVersionInfoSizeW and outlives every pointer
    // VerQueryValueW returns into it; lengths come from the API.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return Vec::new();
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, block.as_mut_ptr().cast())
            .is_err()
        {
            return Vec::new();
        }
        let query = |path: &str| -> Option<(*const u8, usize)> {
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            VerQueryValueW(
                block.as_ptr().cast(),
                &HSTRING::from(path),
                &mut ptr,
                &mut len,
            )
            .as_bool()
            .then_some((ptr.cast_const().cast::<u8>(), len as usize))
            .filter(|(p, _)| !p.is_null())
        };
        // Language/code page pairs, plus the usual US English fallbacks.
        let mut translations: Vec<(u16, u16)> = Vec::new();
        if let Some((ptr, len)) = query("\\VarFileInfo\\Translation") {
            let bytes = std::slice::from_raw_parts(ptr, len);
            for pair in bytes.as_chunks::<4>().0 {
                translations.push((
                    u16::from_le_bytes([pair[0], pair[1]]),
                    u16::from_le_bytes([pair[2], pair[3]]),
                ));
            }
        }
        translations.extend([(0x0409, 0x04B0), (0x0409, 0x04E4)]);
        let mut names = Vec::new();
        for key in ["FileDescription", "ProductName"] {
            for (lang, cp) in &translations {
                let path = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{key}");
                if let Some((ptr, len)) = query(&path) {
                    // `len` counts UTF-16 units, including the terminator.
                    let units = std::slice::from_raw_parts(ptr.cast::<u16>(), len);
                    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
                    let text = String::from_utf16_lossy(&units[..end]);
                    if !text.trim().is_empty() {
                        names.push(text);
                        break;
                    }
                }
            }
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(exe: &str, version: &[&str]) -> Option<String> {
        let version: Vec<String> = version.iter().map(|s| s.to_string()).collect();
        game_name(Path::new(exe), &version)
    }

    #[test]
    fn cleans_trademarks_and_whitespace() {
        assert_eq!(
            clean_name("Halo™   Infinite").as_deref(),
            Some("Halo Infinite")
        );
        assert_eq!(
            clean_name("Rocket League®").as_deref(),
            Some("Rocket League")
        );
        assert_eq!(
            clean_name("Tom Clancy's(TM) Rainbow Six(R)").as_deref(),
            Some("Tom Clancy's Rainbow Six")
        );
        assert_eq!(clean_name("  \t ").as_deref(), None);
        assert_eq!(clean_name("...hidden").as_deref(), Some("hidden"));
        assert_eq!(
            clean_name("Game: Remastered?").as_deref(),
            Some("Game Remastered")
        );
        assert_eq!(
            clean_name("Assassin&apos;s Creed &amp; Co").as_deref(),
            Some("Assassin's Creed & Co")
        );
    }

    #[test]
    fn strips_unreal_suffixes() {
        assert_eq!(
            clean_name("FortniteClient-Win64-Shipping").as_deref(),
            Some("FortniteClient")
        );
        assert_eq!(
            clean_name("VALORANT-Win64-Shipping").as_deref(),
            Some("VALORANT")
        );
        assert_eq!(
            clean_name("Palworld-WinGDK-Shipping").as_deref(),
            Some("Palworld")
        );
        assert_eq!(clean_name("Shipping").as_deref(), Some("Shipping"));
        assert_eq!(clean_name("My-Game").as_deref(), Some("My-Game"));
    }

    #[test]
    fn prefers_version_info_then_exe_name() {
        assert_eq!(
            name(r"C:\G\cs2.exe", &["Counter-Strike 2"]).as_deref(),
            Some("Counter-Strike 2")
        );
        // Generic descriptions fall through to the product name, then the exe.
        assert_eq!(
            name(
                r"C:\G\Hogwarts.exe",
                &["BootstrapPackagedGame", "Hogwarts Legacy"]
            )
            .as_deref(),
            Some("Hogwarts Legacy")
        );
        assert_eq!(
            name(
                r"C:\G\Lyra-Win64-Shipping.exe",
                &["Unreal Engine", "Unreal Engine"]
            )
            .as_deref(),
            Some("Lyra")
        );
        assert_eq!(
            name(r"C:\G\RocketLeague.exe", &[]).as_deref(),
            Some("RocketLeague")
        );
    }

    /// Runs the real lookup on whatever is in front (and on a few system executables).
    #[test]
    #[ignore = "depends on the foreground window; run with --ignored --nocapture"]
    fn reads_foreground_and_version_info() {
        println!("foreground: {:?}", foreground_exe());
        println!("game: {:?}", foreground_game());
        for exe in [
            r"C:\Windows\explorer.exe",
            r"C:\Windows\System32\notepad.exe",
            r"C:\Program Files\Git\git-bash.exe",
        ] {
            println!("{exe}: {:?}", version_names(Path::new(exe)));
        }
        if let Some(exe) = foreground_exe() {
            println!("foreground version names: {:?}", version_names(&exe));
        }
        let names = version_names(Path::new(r"C:\Windows\explorer.exe"));
        assert!(!names.is_empty(), "version info must be readable");
        let explorer = Path::new(r"C:\Windows\explorer.exe");
        assert_eq!(game_name(explorer, &version_names(explorer)), None);
    }

    #[test]
    fn desktop_processes_are_not_games() {
        for exe in [
            r"C:\Windows\explorer.exe",
            r"C:\Windows\SystemApps\SearchHost.exe",
            r"C:\Windows\SystemApps\StartMenuExperienceHost.exe",
            r"C:\Windows\System32\ApplicationFrameHost.exe",
            r"C:\Windows\System32\LockApp.exe",
            r"C:\Windows\System32\dwm.exe",
            r"C:\Users\me\AppData\Local\Krak\krak.exe",
            r"C:\Program Files\Kryo Shard\kryo-shard.exe",
        ] {
            assert_eq!(name(exe, &["Something"]), None, "{exe}");
        }
    }
}
