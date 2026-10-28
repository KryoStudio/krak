//! User settings: defaults, clamping, partial updates and persistence (`settings.json`).

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use tauri_plugin_global_shortcut::Shortcut;

use crate::persist;

pub const DEFAULT_HOTKEY: &str = "F9";
const DEFAULT_BUFFER_SECONDS: u32 = 30;
const MIN_BUFFER_SECONDS: u32 = 10;
const MAX_BUFFER_SECONDS: u32 = 300;
const DEFAULT_FPS: u32 = 60;
const DEFAULT_ACCENT: &str = "glacier";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Balanced,
    #[default]
    High,
    Ultra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Fr,
    En,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Tauri accelerator string, e.g. "F9" or "Ctrl+Shift+S".
    pub hotkey: String,
    pub buffer_seconds: u32,
    /// Always 30 or 60.
    pub fps: u32,
    pub quality: Quality,
    /// Absolute path of the clips folder.
    pub clips_dir: String,
    /// `None` follows the system language.
    pub language: Option<Language>,
    /// UI accent id, opaque to the backend.
    pub accent: String,
    pub sidebar_collapsed: bool,
    pub launch_at_startup: bool,
    pub close_to_tray: bool,
    /// Record the default microphone (extra "Mix" and "Microphone" tracks).
    pub microphone: bool,
    /// Play a short sound when a clip is saved.
    pub capture_sound: bool,
    /// Show the in-game overlay (stored here, used by the overlay window).
    pub overlay: bool,
}

/// `Partial<Settings>` sent by the frontend. Numbers are accepted as floats and clamped.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub hotkey: Option<String>,
    pub buffer_seconds: Option<f64>,
    pub fps: Option<f64>,
    pub quality: Option<Quality>,
    pub clips_dir: Option<String>,
    /// Outer `None` = field absent, `Some(None)` = explicit `null` (follow system).
    #[serde(default, deserialize_with = "present")]
    pub language: Option<Option<Language>>,
    pub accent: Option<String>,
    pub sidebar_collapsed: Option<bool>,
    pub launch_at_startup: Option<bool>,
    pub close_to_tray: Option<bool>,
    pub microphone: Option<bool>,
    pub capture_sound: Option<bool>,
    pub overlay: Option<bool>,
}

/// Distinguishes an explicit `null` from a missing field.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl Settings {
    pub fn with_clips_dir(clips_dir: String) -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            buffer_seconds: DEFAULT_BUFFER_SECONDS,
            fps: DEFAULT_FPS,
            quality: Quality::default(),
            clips_dir,
            language: None,
            accent: DEFAULT_ACCENT.into(),
            sidebar_collapsed: false,
            launch_at_startup: false,
            close_to_tray: true,
            microphone: false,
            capture_sound: true,
            overlay: true,
        }
    }

    /// Returns a copy with `patch` applied and numbers clamped. The hotkey and the clips folder
    /// are only trimmed here: validating them needs the OS and is done by the caller.
    pub fn merge(&self, patch: SettingsPatch) -> Self {
        let mut next = self.clone();
        if let Some(hotkey) = patch.hotkey {
            next.hotkey = hotkey.trim().to_string();
        }
        if let Some(seconds) = patch.buffer_seconds {
            next.buffer_seconds = clamp_buffer_seconds(seconds);
        }
        if let Some(fps) = patch.fps {
            next.fps = snap_fps(fps);
        }
        if let Some(quality) = patch.quality {
            next.quality = quality;
        }
        if let Some(dir) = patch.clips_dir {
            next.clips_dir = dir.trim().to_string();
        }
        if let Some(language) = patch.language {
            next.language = language;
        }
        if let Some(accent) = patch.accent.as_deref().map(str::trim)
            && !accent.is_empty()
        {
            next.accent = accent.to_string();
        }
        if let Some(collapsed) = patch.sidebar_collapsed {
            next.sidebar_collapsed = collapsed;
        }
        if let Some(launch) = patch.launch_at_startup {
            next.launch_at_startup = launch;
        }
        if let Some(close_to_tray) = patch.close_to_tray {
            next.close_to_tray = close_to_tray;
        }
        if let Some(microphone) = patch.microphone {
            next.microphone = microphone;
        }
        if let Some(capture_sound) = patch.capture_sound {
            next.capture_sound = capture_sound;
        }
        if let Some(overlay) = patch.overlay {
            next.overlay = overlay;
        }
        next
    }
}

pub fn clamp_buffer_seconds(seconds: f64) -> u32 {
    if !seconds.is_finite() {
        return DEFAULT_BUFFER_SECONDS;
    }
    // Clamped first, so the cast cannot truncate.
    seconds
        .round()
        .clamp(f64::from(MIN_BUFFER_SECONDS), f64::from(MAX_BUFFER_SECONDS)) as u32
}

/// Only 30 and 60 fps are supported: snap anything else to the closest one.
pub fn snap_fps(fps: f64) -> u32 {
    if !fps.is_finite() {
        DEFAULT_FPS
    } else if fps < 45.0 {
        30
    } else {
        60
    }
}

/// Parses a settings file leniently: unknown keys are ignored and every missing or invalid field
/// falls back to its default, so one bad value never resets everything. `None` = not a JSON object.
pub fn parse(text: &str, defaults: Settings) -> Option<Settings> {
    let serde_json::Value::Object(map) = serde_json::from_str(text).ok()? else {
        return None;
    };
    let default_dir = defaults.clips_dir.clone();
    let mut settings = defaults;
    for (key, value) in map {
        let field = serde_json::Value::Object(serde_json::Map::from_iter([(key, value)]));
        if let Ok(patch) = serde_json::from_value::<SettingsPatch>(field) {
            settings = settings.merge(patch);
        }
    }
    if Shortcut::from_str(&settings.hotkey).is_err() {
        settings.hotkey = DEFAULT_HOTKEY.into();
    }
    if !Path::new(&settings.clips_dir).is_absolute() {
        settings.clips_dir = default_dir;
    }
    Some(settings)
}

/// Loads settings; a missing or corrupt file yields defaults (a corrupt one is kept as `.bak`).
pub fn load(path: &Path, default_clips_dir: String) -> Settings {
    let defaults = Settings::with_clips_dir(default_clips_dir);
    match persist::read(path) {
        persist::Read::Text(text) => match parse(&text, defaults.clone()) {
            Some(settings) => settings,
            None => {
                persist::backup(path);
                let _ = save(path, &defaults);
                defaults
            }
        },
        persist::Read::Missing => {
            let _ = save(path, &defaults);
            defaults
        }
        persist::Read::Unreadable => defaults,
    }
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    persist::write_json(path, settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Settings {
        Settings::with_clips_dir(r"C:\Users\me\Videos\Kryo Shard".into())
    }

    fn patch(json: &str) -> SettingsPatch {
        serde_json::from_str(json).expect("valid patch")
    }

    #[test]
    fn defaults_match_contract() {
        let s = defaults();
        assert_eq!(s.hotkey, "F9");
        assert_eq!(s.buffer_seconds, 30);
        assert_eq!(s.fps, 60);
        assert_eq!(s.quality, Quality::High);
        assert_eq!(s.language, None);
        assert_eq!(s.accent, "glacier");
        assert!(!s.sidebar_collapsed && !s.launch_at_startup && s.close_to_tray);
        assert!(!s.microphone && s.capture_sound && s.overlay);
    }

    #[test]
    fn serializes_camel_case() {
        let json = serde_json::to_value(defaults()).expect("serializable");
        assert_eq!(json["bufferSeconds"], 30);
        assert_eq!(json["clipsDir"], r"C:\Users\me\Videos\Kryo Shard");
        assert_eq!(json["quality"], "high");
        assert!(json["language"].is_null());
        assert_eq!(json["closeToTray"], true);
        assert_eq!(json["sidebarCollapsed"], false);
        assert_eq!(json["microphone"], false);
        assert_eq!(json["captureSound"], true);
        assert_eq!(json["overlay"], true);
    }

    #[test]
    fn merges_capture_toggles() {
        let s = defaults().merge(patch(
            r#"{"microphone": true, "captureSound": false, "overlay": false}"#,
        ));
        assert!(s.microphone && !s.capture_sound && !s.overlay);
        assert_eq!(s.merge(patch("{}")), s);
    }

    #[test]
    fn clamps_buffer_seconds() {
        assert_eq!(clamp_buffer_seconds(5.0), 10);
        assert_eq!(clamp_buffer_seconds(-3.0), 10);
        assert_eq!(clamp_buffer_seconds(1000.0), 300);
        assert_eq!(clamp_buffer_seconds(29.6), 30);
        assert_eq!(clamp_buffer_seconds(f64::NAN), 30);
        assert_eq!(clamp_buffer_seconds(f64::INFINITY), 30);
    }

    #[test]
    fn snaps_fps() {
        assert_eq!(snap_fps(30.0), 30);
        assert_eq!(snap_fps(24.0), 30);
        assert_eq!(snap_fps(45.0), 60);
        assert_eq!(snap_fps(144.0), 60);
        assert_eq!(snap_fps(f64::NAN), 60);
    }

    #[test]
    fn merge_applies_and_clamps() {
        let s = defaults().merge(patch(
            r#"{"bufferSeconds": 9999, "fps": 30, "quality": "ultra", "language": "fr", "hotkey": " Ctrl+F9 "}"#,
        ));
        assert_eq!(s.buffer_seconds, 300);
        assert_eq!(s.fps, 30);
        assert_eq!(s.quality, Quality::Ultra);
        assert_eq!(s.language, Some(Language::Fr));
        assert_eq!(s.hotkey, "Ctrl+F9");
    }

    #[test]
    fn merge_distinguishes_null_language_from_missing() {
        let fr = defaults().merge(patch(r#"{"language": "fr"}"#));
        assert_eq!(fr.merge(patch("{}")).language, Some(Language::Fr));
        assert_eq!(fr.merge(patch(r#"{"language": null}"#)).language, None);
    }

    #[test]
    fn merge_ignores_blank_accent() {
        assert_eq!(
            defaults().merge(patch(r#"{"accent": "  "}"#)).accent,
            "glacier"
        );
        assert_eq!(
            defaults().merge(patch(r#"{"accent": "ember"}"#)).accent,
            "ember"
        );
    }

    #[test]
    fn parse_is_lenient_per_field() {
        let s = parse(
            r#"{"quality": "potato", "fps": 30, "unknown": 1, "bufferSeconds": "x", "closeToTray": false, "microphone": "yes", "captureSound": false}"#,
            defaults(),
        )
        .expect("object");
        assert_eq!(s.quality, Quality::High);
        assert_eq!(s.fps, 30);
        assert_eq!(s.buffer_seconds, 30);
        assert!(!s.close_to_tray);
        // Invalid values fall back to the default, valid ones stick; missing ones default.
        assert!(!s.microphone && !s.capture_sound && s.overlay);
    }

    #[test]
    fn parse_resets_invalid_hotkey_and_relative_dir() {
        let s = parse(
            r#"{"hotkey": "Ctrl+", "clipsDir": "relative\\dir"}"#,
            defaults(),
        )
        .expect("object");
        assert_eq!(s.hotkey, "F9");
        assert_eq!(s.clips_dir, defaults().clips_dir);
    }

    #[test]
    fn parse_rejects_non_objects() {
        assert!(parse("{oops", defaults()).is_none());
        assert!(parse("[]", defaults()).is_none());
        assert!(parse("", defaults()).is_none());
    }

    #[test]
    fn load_backs_up_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("kryo-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{not json").expect("write");

        let s = load(&path, defaults().clips_dir);
        assert_eq!(s, defaults());
        let backup = std::fs::read_to_string(dir.join("settings.json.bak")).expect("backup");
        assert_eq!(backup, "{not json");
        let rewritten = std::fs::read_to_string(&path).expect("rewritten");
        assert_eq!(parse(&rewritten, defaults()), Some(defaults()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
