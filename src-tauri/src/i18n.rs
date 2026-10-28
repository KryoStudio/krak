//! The few strings the backend shows itself (tray menu, folder picker, desktop clip names,
//! edited copies).

use crate::settings::Language;

pub struct Labels {
    pub open_app: &'static str,
    pub capture_now: &'static str,
    pub pause_replay: &'static str,
    pub resume_replay: &'static str,
    pub open_clips_dir: &'static str,
    pub settings: &'static str,
    pub quit: &'static str,
    pub choose_clips_dir: &'static str,
    /// Name of clips taken outside a game (file name prefix).
    pub desktop: &'static str,
    /// Suffix of an edited copy: "<clip> (<edited>).mp4".
    pub edited: &'static str,
}

const EN: Labels = Labels {
    open_app: "Open Kryo Shard",
    capture_now: "Capture now",
    pause_replay: "Pause replay",
    resume_replay: "Resume replay",
    open_clips_dir: "Open clips folder",
    settings: "Settings",
    quit: "Quit",
    choose_clips_dir: "Choose the clips folder",
    desktop: "Desktop",
    edited: "edit",
};

const FR: Labels = Labels {
    open_app: "Ouvrir Kryo Shard",
    capture_now: "Capturer maintenant",
    pause_replay: "Mettre le replay en pause",
    resume_replay: "Reprendre le replay",
    open_clips_dir: "Ouvrir le dossier des clips",
    settings: "Paramètres",
    quit: "Quitter",
    choose_clips_dir: "Choisir le dossier des clips",
    desktop: "Bureau",
    edited: "montage",
};

/// The configured language, or the system one (French for any `fr-*` locale, else English).
pub fn effective(language: Option<Language>) -> Language {
    language.unwrap_or_else(|| match sys_locale::get_locale() {
        Some(locale) if locale.to_ascii_lowercase().starts_with("fr") => Language::Fr,
        _ => Language::En,
    })
}

pub fn labels(language: Option<Language>) -> &'static Labels {
    match effective(language) {
        Language::Fr => &FR,
        Language::En => &EN,
    }
}
