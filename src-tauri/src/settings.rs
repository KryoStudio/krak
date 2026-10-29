//! User settings: defaults, clamping, partial updates and persistence (`settings.json`).

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use tauri_plugin_global_shortcut::Shortcut;

use crate::persist;

pub const DEFAULT_HOTKEY: &str = "F9";
/// Not Alt+F9: that is NVIDIA's own recording shortcut, often already taken.
pub const DEFAULT_RECORD_HOTKEY: &str = "Shift+F9";
pub const DEFAULT_SCREENSHOT_HOTKEY: &str = "Ctrl+F9";
const DEFAULT_BUFFER_SECONDS: u32 = 30;
const MIN_BUFFER_SECONDS: u32 = 10;
const MAX_BUFFER_SECONDS: u32 = 300;
const DEFAULT_FPS: u32 = 60;
/// Frame rates offered; the engine caps them to the display's refresh rate.
const FPS_CHOICES: [u32; 4] = [30, 60, 120, 144];
const DEFAULT_ACCENT: &str = "glacier";
/// Custom bitrate, Mbit/s.
const DEFAULT_BITRATE: u32 = 40;
const MIN_BITRATE: u32 = 2;
const MAX_BITRATE: u32 = 150;
/// AAC kbit/s of the stereo tracks.
pub const AUDIO_BITRATES: [u32; 5] = [128, 160, 192, 256, 320];
const DEFAULT_AUDIO_BITRATE: u32 = 192;
/// Storage limit bounds, GB.
const MIN_STORAGE_GB: f64 = 1.0;
const MAX_STORAGE_GB: f64 = 100_000.0;

/// Older settings files said "balanced" / "ultra": read as standard / premium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[serde(alias = "balanced")]
    Standard,
    #[default]
    High,
    #[serde(alias = "ultra")]
    Premium,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecChoice {
    #[default]
    Auto,
    H264,
    Hevc,
    Av1,
}

/// Cap on the short side of the recording (never upscaled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Resolution {
    #[default]
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "1440")]
    P1440,
    #[serde(rename = "1080")]
    P1080,
    #[serde(rename = "720")]
    P720,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Tauri accelerator string, e.g. "F9" or "Ctrl+Shift+S".
    pub hotkey: String,
    /// Starts and stops a continuous recording.
    pub record_hotkey: String,
    pub screenshot_hotkey: String,
    pub buffer_seconds: u32,
    pub quality: Quality,
    /// Custom quality only ("auto" = premium's choice).
    pub codec: CodecChoice,
    /// Custom quality only.
    pub resolution: Resolution,
    /// 30, 60, 120 or 144 (capped to the refresh rate by the engine).
    pub fps: u32,
    /// Mbit/s, custom quality only.
    pub bitrate: u32,
    /// Custom quality only, HEVC/AV1.
    pub ten_bit: bool,
    /// AAC kbit/s of the Game and Mix tracks (the microphone stays mono 96).
    pub audio_bitrate: u32,
    /// Absolute path of the clips folder.
    pub clips_dir: String,
    /// Interface language code ("en", "pt"…, see src/lib/locales); `None` follows the system.
    /// Kept even when this version doesn't know it (the interface then follows the system).
    pub language: Option<String>,
    /// UI accent id, opaque to the backend.
    pub accent: String,
    pub sidebar_collapsed: bool,
    pub launch_at_startup: bool,
    pub close_to_tray: bool,
    /// Record a microphone (extra "Mix" and "Microphone" tracks).
    pub microphone: bool,
    /// WASAPI endpoint id; `None` = the Windows default capture device.
    pub microphone_device: Option<String>,
    /// Play a short sound when a clip is saved.
    pub capture_sound: bool,
    /// Show the in-game overlay (stored here, used by the overlay window).
    pub overlay: bool,
    /// Show "Clipping <game>" on the user's Discord profile.
    pub discord_presence: bool,
    /// Burn a small "by krak" badge into exports.
    pub watermark: bool,
    /// Oldest non-favorite clips go to the Recycle Bin beyond this many GB; `None` = no limit.
    pub storage_limit_gb: Option<f64>,
    /// The first-run wizard is done.
    pub onboarded: bool,
}

/// `Partial<Settings>` sent by the frontend. Numbers are accepted as floats and clamped.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub hotkey: Option<String>,
    pub record_hotkey: Option<String>,
    pub screenshot_hotkey: Option<String>,
    pub buffer_seconds: Option<f64>,
    pub fps: Option<f64>,
    pub quality: Option<Quality>,
    pub codec: Option<CodecChoice>,
    pub resolution: Option<Resolution>,
    pub bitrate: Option<f64>,
    pub ten_bit: Option<bool>,
    pub audio_bitrate: Option<f64>,
    pub clips_dir: Option<String>,
    /// Outer `None` = field absent, `Some(None)` = explicit `null` (follow system).
    #[serde(default, deserialize_with = "present")]
    pub language: Option<Option<String>>,
    pub accent: Option<String>,
    pub sidebar_collapsed: Option<bool>,
    pub launch_at_startup: Option<bool>,
    pub close_to_tray: Option<bool>,
    pub microphone: Option<bool>,
    /// Outer `None` = absent, `Some(None)` = explicit `null` (Windows default).
    #[serde(default, deserialize_with = "present")]
    pub microphone_device: Option<Option<String>>,
    pub capture_sound: Option<bool>,
    pub overlay: Option<bool>,
    pub discord_presence: Option<bool>,
    pub watermark: Option<bool>,
    /// Outer `None` = absent, `Some(None)` = explicit `null` (no limit).
    #[serde(default, deserialize_with = "present")]
    pub storage_limit_gb: Option<Option<f64>>,
    pub onboarded: Option<bool>,
}

/// Distinguishes an explicit `null` from a missing field.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl Settings {
    pub fn with_clips_dir(clips_dir: String) -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            record_hotkey: DEFAULT_RECORD_HOTKEY.into(),
            screenshot_hotkey: DEFAULT_SCREENSHOT_HOTKEY.into(),
            buffer_seconds: DEFAULT_BUFFER_SECONDS,
            quality: Quality::default(),
            codec: CodecChoice::default(),
            resolution: Resolution::default(),
            fps: DEFAULT_FPS,
            bitrate: DEFAULT_BITRATE,
            ten_bit: true,
            audio_bitrate: DEFAULT_AUDIO_BITRATE,
            clips_dir,
            language: None,
            accent: DEFAULT_ACCENT.into(),
            sidebar_collapsed: false,
            launch_at_startup: false,
            close_to_tray: true,
            microphone: false,
            microphone_device: None,
            capture_sound: true,
            overlay: true,
            discord_presence: true,
            watermark: false,
            storage_limit_gb: None,
            onboarded: false,
        }
    }

    /// Returns a copy with `patch` applied and numbers clamped. The hotkeys and the clips folder
    /// are only trimmed here: validating them needs the OS and is done by the caller.
    pub fn merge(&self, patch: SettingsPatch) -> Self {
        let mut next = self.clone();
        if let Some(hotkey) = patch.hotkey {
            next.hotkey = hotkey.trim().to_string();
        }
        if let Some(hotkey) = patch.record_hotkey {
            next.record_hotkey = hotkey.trim().to_string();
        }
        if let Some(hotkey) = patch.screenshot_hotkey {
            next.screenshot_hotkey = hotkey.trim().to_string();
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
        if let Some(codec) = patch.codec {
            next.codec = codec;
        }
        if let Some(resolution) = patch.resolution {
            next.resolution = resolution;
        }
        if let Some(bitrate) = patch.bitrate {
            next.bitrate = clamp_bitrate(bitrate);
        }
        if let Some(ten_bit) = patch.ten_bit {
            next.ten_bit = ten_bit;
        }
        if let Some(kbps) = patch.audio_bitrate {
            next.audio_bitrate = snap_audio_bitrate(kbps);
        }
        if let Some(dir) = patch.clips_dir {
            next.clips_dir = dir.trim().to_string();
        }
        if let Some(language) = patch.language {
            next.language = language.as_deref().and_then(language_code);
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
        if let Some(device) = patch.microphone_device {
            next.microphone_device = device
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty());
        }
        if let Some(limit) = patch.storage_limit_gb {
            next.storage_limit_gb = limit.and_then(clamp_storage_gb);
        }
        if let Some(onboarded) = patch.onboarded {
            next.onboarded = onboarded;
        }
        if let Some(capture_sound) = patch.capture_sound {
            next.capture_sound = capture_sound;
        }
        if let Some(overlay) = patch.overlay {
            next.overlay = overlay;
        }
        if let Some(presence) = patch.discord_presence {
            next.discord_presence = presence;
        }
        if let Some(watermark) = patch.watermark {
            next.watermark = watermark;
        }
        next
    }
}

/// A language code as the interface writes it ("fr", "pt", "zh-CN"), lowercased; `None` if
/// it isn't one (then the system language is used).
fn language_code(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_lowercase();
    let mut parts = code.split('-');
    let primary = parts.next()?;
    let valid = (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts
            .all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()));
    valid.then_some(code)
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

/// 30, 60, 120 or 144 fps: anything else snaps to the closest one.
pub fn snap_fps(fps: f64) -> u32 {
    snap(fps, &FPS_CHOICES, DEFAULT_FPS)
}

/// 128, 160, 192, 256 or 320 kbit/s: anything else snaps to the closest one.
pub fn snap_audio_bitrate(kbps: f64) -> u32 {
    snap(kbps, &AUDIO_BITRATES, DEFAULT_AUDIO_BITRATE)
}

fn snap(value: f64, choices: &[u32], default: u32) -> u32 {
    if !value.is_finite() {
        return default;
    }
    choices
        .iter()
        .copied()
        .min_by(|a, b| {
            (f64::from(*a) - value)
                .abs()
                .total_cmp(&(f64::from(*b) - value).abs())
        })
        .unwrap_or(default)
}

/// Mbit/s, 2..=150.
pub fn clamp_bitrate(mbps: f64) -> u32 {
    if !mbps.is_finite() {
        return DEFAULT_BITRATE;
    }
    mbps.round()
        .clamp(f64::from(MIN_BITRATE), f64::from(MAX_BITRATE)) as u32
}

/// GB, 1..=100 000 (one decimal kept); NaN means no limit.
pub fn clamp_storage_gb(gb: f64) -> Option<f64> {
    gb.is_finite()
        .then(|| ((gb * 10.0).round() / 10.0).clamp(MIN_STORAGE_GB, MAX_STORAGE_GB))
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
    for (hotkey, default) in [
        (&mut settings.hotkey, DEFAULT_HOTKEY),
        (&mut settings.record_hotkey, DEFAULT_RECORD_HOTKEY),
        (&mut settings.screenshot_hotkey, DEFAULT_SCREENSHOT_HOTKEY),
    ] {
        if Shortcut::from_str(hotkey).is_err() {
            *hotkey = default.into();
        }
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
        Settings::with_clips_dir(r"C:\Users\me\Videos\Krak".into())
    }

    fn patch(json: &str) -> SettingsPatch {
        serde_json::from_str(json).expect("valid patch")
    }

    #[test]
    fn defaults_match_contract() {
        let s = defaults();
        assert_eq!(s.hotkey, "F9");
        assert_eq!(
            (s.record_hotkey.as_str(), s.screenshot_hotkey.as_str()),
            ("Shift+F9", "Ctrl+F9")
        );
        assert_eq!(s.buffer_seconds, 30);
        assert_eq!(s.fps, 60);
        assert_eq!(s.quality, Quality::High);
        assert_eq!(
            (s.codec, s.resolution),
            (CodecChoice::Auto, Resolution::Native)
        );
        assert_eq!((s.bitrate, s.ten_bit, s.audio_bitrate), (40, true, 192));
        assert_eq!((s.microphone_device, s.storage_limit_gb), (None, None));
        assert!(!s.onboarded);
        assert_eq!(s.language, None);
        assert_eq!(s.accent, "glacier");
        assert!(!s.sidebar_collapsed && !s.launch_at_startup && s.close_to_tray);
        assert!(!s.microphone && s.capture_sound && s.overlay);
        assert!(s.discord_presence && !s.watermark);
    }

    #[test]
    fn serializes_camel_case() {
        let json = serde_json::to_value(defaults()).expect("serializable");
        assert_eq!(json["bufferSeconds"], 30);
        assert_eq!(json["clipsDir"], r"C:\Users\me\Videos\Krak");
        assert_eq!(json["quality"], "high");
        assert!(json["language"].is_null());
        assert_eq!(json["closeToTray"], true);
        assert_eq!(json["sidebarCollapsed"], false);
        assert_eq!(json["microphone"], false);
        assert_eq!(json["captureSound"], true);
        assert_eq!(json["overlay"], true);
        assert_eq!(json["codec"], "auto");
        assert_eq!(json["resolution"], "native");
        assert_eq!(json["bitrate"], 40);
        assert_eq!(json["tenBit"], true);
        assert_eq!(json["audioBitrate"], 192);
        assert!(json["microphoneDevice"].is_null());
        assert!(json["storageLimitGb"].is_null());
        assert_eq!(json["onboarded"], false);
        assert_eq!(json["recordHotkey"], "Shift+F9");
        assert_eq!(json["screenshotHotkey"], "Ctrl+F9");
        assert_eq!(json["discordPresence"], true);
        assert_eq!(json["watermark"], false);
        let custom = defaults().merge(patch(r#"{"quality": "custom", "resolution": "1440"}"#));
        let json = serde_json::to_value(custom).expect("serializable");
        assert_eq!(
            (json["quality"].as_str(), json["resolution"].as_str()),
            (Some("custom"), Some("1440"))
        );
    }

    #[test]
    fn migrates_old_quality_names() {
        let old = |q: &str| {
            parse(&format!(r#"{{"quality": "{q}"}}"#), defaults())
                .expect("object")
                .quality
        };
        assert_eq!(old("balanced"), Quality::Standard);
        assert_eq!(old("high"), Quality::High);
        assert_eq!(old("ultra"), Quality::Premium);
        assert_eq!(old("premium"), Quality::Premium);
        assert_eq!(old("potato"), Quality::High);
        // Written back with the new names.
        let s = defaults().merge(patch(r#"{"quality": "ultra"}"#));
        assert_eq!(serde_json::to_value(s).unwrap()["quality"], "premium");
    }

    #[test]
    fn merges_the_video_and_audio_fields() {
        let s = defaults().merge(patch(
            r#"{"codec": "hevc", "resolution": "720", "fps": 144, "bitrate": 0.4, "tenBit": false, "audioBitrate": 300}"#,
        ));
        assert_eq!(
            (s.codec, s.resolution, s.fps),
            (CodecChoice::Hevc, Resolution::P720, 144)
        );
        assert_eq!((s.bitrate, s.ten_bit, s.audio_bitrate), (2, false, 320));
        assert_eq!(s.merge(patch(r#"{"bitrate": 1000}"#)).bitrate, 150);
        assert_eq!(
            s.merge(patch(r#"{"audioBitrate": 140}"#)).audio_bitrate,
            128
        );
        assert_eq!(snap_audio_bitrate(f64::NAN), 192);
        // Unknown enum values are refused (the field keeps its value).
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"codec": "vp9"}"#).is_err());
    }

    #[test]
    fn merges_device_and_storage_with_explicit_nulls() {
        let s = defaults().merge(patch(
            r#"{"microphoneDevice": " {0.0.1.00000000}.{abc} ", "storageLimitGb": 49.97, "onboarded": true}"#,
        ));
        assert_eq!(
            s.microphone_device.as_deref(),
            Some("{0.0.1.00000000}.{abc}")
        );
        assert_eq!(s.storage_limit_gb, Some(50.0));
        assert!(s.onboarded);
        assert_eq!(s.merge(patch("{}")), s, "absent = unchanged");
        let cleared = s.merge(patch(
            r#"{"microphoneDevice": null, "storageLimitGb": null}"#,
        ));
        assert_eq!(
            (cleared.microphone_device, cleared.storage_limit_gb),
            (None, None)
        );
        assert_eq!(
            s.merge(patch(r#"{"microphoneDevice": "  "}"#))
                .microphone_device,
            None
        );
        assert_eq!(
            s.merge(patch(r#"{"storageLimitGb": 0}"#)).storage_limit_gb,
            Some(1.0)
        );
        assert_eq!(clamp_storage_gb(f64::INFINITY), None);
    }

    #[test]
    fn merges_capture_toggles() {
        let s = defaults().merge(patch(
            r#"{"microphone": true, "captureSound": false, "overlay": false}"#,
        ));
        assert!(s.microphone && !s.capture_sound && !s.overlay);
        assert_eq!(s.merge(patch("{}")), s);
        let s = s.merge(patch(
            r#"{"discordPresence": false, "watermark": true, "recordHotkey": " Ctrl+Alt+R ", "screenshotHotkey": "F10"}"#,
        ));
        assert!(!s.discord_presence && s.watermark);
        assert_eq!(
            (s.record_hotkey.as_str(), s.screenshot_hotkey.as_str()),
            ("Ctrl+Alt+R", "F10")
        );
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
        assert_eq!(snap_fps(50.0), 60);
        assert_eq!(snap_fps(90.0), 60, "a tie goes to the lower rate");
        assert_eq!(snap_fps(100.0), 120);
        assert_eq!(snap_fps(144.0), 144);
        assert_eq!(snap_fps(240.0), 144);
        assert_eq!(snap_fps(f64::NAN), 60);
    }

    #[test]
    fn merge_applies_and_clamps() {
        let s = defaults().merge(patch(
            r#"{"bufferSeconds": 9999, "fps": 30, "quality": "premium", "language": "fr", "hotkey": " Ctrl+F9 "}"#,
        ));
        assert_eq!(s.buffer_seconds, 300);
        assert_eq!(s.fps, 30);
        assert_eq!(s.quality, Quality::Premium);
        assert_eq!(s.language.as_deref(), Some("fr"));
        assert_eq!(s.hotkey, "Ctrl+F9");
    }

    #[test]
    fn merge_distinguishes_null_language_from_missing() {
        let fr = defaults().merge(patch(r#"{"language": "fr"}"#));
        assert_eq!(fr.merge(patch("{}")).language.as_deref(), Some("fr"));
        assert_eq!(fr.merge(patch(r#"{"language": null}"#)).language, None);
    }

    #[test]
    fn keeps_any_language_code() {
        let lang = |code: &str| {
            defaults()
                .merge(patch(&format!(r#"{{"language": "{code}"}}"#)))
                .language
        };
        // Languages this version doesn't know are kept: a newer interface may have them.
        assert_eq!(lang("pt").as_deref(), Some("pt"));
        assert_eq!(lang(" zh-CN ").as_deref(), Some("zh-cn"));
        assert_eq!(lang("tlh").as_deref(), Some("tlh"));
        for invalid in ["", "x", "english", "fr_FR", "../fr", "fr-", "é"] {
            assert_eq!(lang(invalid), None, "{invalid}");
        }
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
            r#"{"hotkey": "Ctrl+", "recordHotkey": "Nope+1", "screenshotHotkey": "", "clipsDir": "relative\\dir"}"#,
            defaults(),
        )
        .expect("object");
        assert_eq!(s.hotkey, "F9");
        assert_eq!(
            (s.record_hotkey.as_str(), s.screenshot_hotkey.as_str()),
            ("Shift+F9", "Ctrl+F9")
        );
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
        let dir = std::env::temp_dir().join(format!("krak-settings-test-{}", std::process::id()));
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
