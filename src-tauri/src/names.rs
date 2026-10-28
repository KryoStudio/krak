//! Clip file names: Windows-safe sanitizing and auto-generated title detection.

/// Longest title (in characters) accepted for a renamed clip.
pub const MAX_TITLE_CHARS: usize = 120;

/// Device names Windows refuses as file names, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turns a user-typed title into a valid Windows file stem, or `None` if nothing usable is left.
pub fn sanitize_title(input: &str) -> Option<String> {
    let cleaned: String = input
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .collect();
    let truncated: String = cleaned.trim().chars().take(MAX_TITLE_CHARS).collect();
    // Windows silently drops trailing dots and spaces, which would change the name behind our back.
    let name = truncated
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_start();
    if name.is_empty() || is_reserved(name) {
        return None;
    }
    Some(name.to_string())
}

/// `CON`, `nul.txt`, `Com1 ` ... are all device names for Windows.
fn is_reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).trim_end();
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(base))
}

/// Display title for a clip: its file stem, or `None` when the name was generated automatically
/// (ends with a `YYYY-MM-DD HH-MM-SS` timestamp, optionally followed by a ` (N)` collision
/// suffix) so the UI can show game + date instead.
pub fn display_title(stem: &str) -> Option<String> {
    (!is_auto_named(stem)).then(|| stem.to_string())
}

fn is_auto_named(stem: &str) -> bool {
    const LEN: usize = "YYYY-MM-DD HH-MM-SS".len();
    let bytes = strip_collision_suffix(stem.as_bytes());
    let Some(split) = bytes.len().checked_sub(LEN) else {
        return false;
    };
    let (head, stamp) = bytes.split_at(split);
    (head.is_empty() || head.ends_with(b" ")) && is_timestamp(stamp)
}

/// `... (12)` -> `...` (1 to 3 digits, as generated on name collisions).
fn strip_collision_suffix(bytes: &[u8]) -> &[u8] {
    let Some(inner) = bytes.strip_suffix(b")") else {
        return bytes;
    };
    let digits = inner
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    match inner[..inner.len() - digits].strip_suffix(b" (") {
        Some(head) if (1..=3).contains(&digits) => head,
        _ => bytes,
    }
}

fn is_timestamp(stamp: &[u8]) -> bool {
    stamp.iter().enumerate().all(|(i, c)| match i {
        4 | 7 | 13 | 16 => *c == b'-',
        10 => *c == b' ',
        _ => c.is_ascii_digit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_forbidden_and_control_chars() {
        assert_eq!(
            sanitize_title(r#"  my<clip>: "ace"/\|?*  "#).as_deref(),
            Some("myclip ace")
        );
        assert_eq!(
            sanitize_title("clutch\u{7}\n\tround").as_deref(),
            Some("clutchround")
        );
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_title("clip... . ").as_deref(), Some("clip"));
        assert_eq!(sanitize_title(".hidden").as_deref(), Some(".hidden"));
    }

    #[test]
    fn rejects_empty_results() {
        assert_eq!(sanitize_title(""), None);
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title("..."), None);
        assert_eq!(sanitize_title("<>:\"/\\|?*"), None);
    }

    #[test]
    fn rejects_reserved_names() {
        for name in ["CON", "con", "Nul", "AUX.txt", "COM1", "lpt9", "PRN "] {
            assert_eq!(sanitize_title(name), None, "{name}");
        }
        assert_eq!(sanitize_title("COM10").as_deref(), Some("COM10"));
        assert_eq!(sanitize_title("CONSOLE").as_deref(), Some("CONSOLE"));
    }

    #[test]
    fn truncates_long_titles() {
        let long = "é".repeat(200);
        assert_eq!(
            sanitize_title(&long).map(|t| t.chars().count()),
            Some(MAX_TITLE_CHARS)
        );
        // A dot landing at the cut is still trimmed.
        let dotted = format!("{}.{}", "a".repeat(MAX_TITLE_CHARS - 1), "b".repeat(10));
        assert_eq!(
            sanitize_title(&dotted),
            Some("a".repeat(MAX_TITLE_CHARS - 1))
        );
    }

    #[test]
    fn detects_auto_timestamps() {
        assert_eq!(display_title("Valorant 2026-10-28 20-15-03"), None);
        assert_eq!(display_title("2026-10-28 20-15-03"), None);
        assert_eq!(display_title("Échec 2026-10-28 20-15-03"), None);
        // Collision suffixes added by the capture engine.
        assert_eq!(display_title("Clip 2026-10-28 20-15-03 (2)"), None);
        assert_eq!(display_title("Bureau 2026-10-28 20-15-03 (123)"), None);
    }

    #[test]
    fn keeps_custom_titles() {
        for stem in [
            "My ace",
            "Ace 2026-10-28",
            "x2026-10-28 20-15-03",
            "Clip 2026-1a-28 20-15-03",
            "Clip 2026-10-28 20-15-03 (x)",
            "Clip 2026-10-28 20-15-03 (1234)",
            "Clip 2026-10-28 20-15-03 ()",
            "Clip 2026-10-28 20-15-03(2)",
            "Clip 2026-10-28_20-15-03",
        ] {
            assert_eq!(display_title(stem).as_deref(), Some(stem), "{stem}");
        }
    }
}
