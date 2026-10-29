//! The few strings the backend shows itself (tray menu, folder picker, file names, Discord
//! status), in every language of the interface (src/lib/locales). The ones the interface
//! shows too are the same words: keep them in step when a translation changes.

/// How a language picks the form of a text with a number, for whole numbers (the rules
/// Intl.PluralRules applies in the interface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// 1 is singular (English, Spanish, German…).
    OneIsOne,
    /// 0 and 1 are singular (French).
    ZeroOrOne,
    /// One form (Japanese, Korean, Chinese).
    Invariable,
    Polish,
    Russian,
}

/// A text with `{count}`, in the forms its language uses (unused ones repeat `other`).
pub struct Plural {
    one: &'static str,
    few: &'static str,
    many: &'static str,
    other: &'static str,
}

impl Rule {
    fn pick(self, plural: &Plural, n: u64) -> &'static str {
        let (units, tens) = (n % 10, n % 100);
        let few = (2..=4).contains(&units) && !(12..=14).contains(&tens);
        match self {
            Rule::OneIsOne if n == 1 => plural.one,
            Rule::ZeroOrOne if n <= 1 => plural.one,
            Rule::Polish if n == 1 => plural.one,
            Rule::Polish if few => plural.few,
            Rule::Polish => plural.many,
            Rule::Russian if units == 1 && tens != 11 => plural.one,
            Rule::Russian if few => plural.few,
            Rule::Russian => plural.many,
            _ => plural.other,
        }
    }
}

pub struct Labels {
    rule: Rule,
    pub open_app: &'static str,
    pub capture_now: &'static str,
    pub pause_replay: &'static str,
    pub resume_replay: &'static str,
    pub start_recording: &'static str,
    pub stop_recording: &'static str,
    pub take_screenshot: &'static str,
    pub open_clips_dir: &'static str,
    pub settings: &'static str,
    pub quit: &'static str,
    pub choose_clips_dir: &'static str,
    /// Name of clips taken outside a game (file name prefix).
    pub desktop: &'static str,
    /// Suffix of an edited copy: "<clip> (<edited>).mp4".
    pub edited: &'static str,
    /// Discord status while a game is in front: "Clipping {game}".
    pub presence_playing: &'static str,
    /// Discord status outside a game.
    pub presence_desktop: &'static str,
    /// Discord status during a continuous recording.
    pub presence_recording: &'static str,
    /// Discord button to the download page.
    pub presence_button: &'static str,
    /// "{count} shards captured today": use [`Labels::shards_today`].
    presence_today: Plural,
}

impl Labels {
    /// "3 shards captured today", in the right form for `count`.
    pub fn shards_today(&self, count: u64) -> String {
        self.rule
            .pick(&self.presence_today, count)
            .replace("{count}", &count.to_string())
    }
}

const EN: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Open Krak",
    capture_now: "Capture now",
    pause_replay: "Pause replay",
    resume_replay: "Resume replay",
    start_recording: "Start recording",
    stop_recording: "Stop recording",
    take_screenshot: "Screenshot",
    open_clips_dir: "Open shards folder",
    settings: "Settings",
    quit: "Quit",
    choose_clips_dir: "Choose the shards folder",
    desktop: "Desktop",
    edited: "edit",
    presence_playing: "Clipping {game}",
    presence_desktop: "On the desktop",
    presence_recording: "Recording their game",
    presence_button: "Get Krak",
    presence_today: Plural {
        one: "{count} shard captured today",
        few: "{count} shards captured today",
        many: "{count} shards captured today",
        other: "{count} shards captured today",
    },
};

const FR: Labels = Labels {
    rule: Rule::ZeroOrOne,
    open_app: "Ouvrir Krak",
    capture_now: "Capturer maintenant",
    pause_replay: "Mettre le replay en pause",
    resume_replay: "Reprendre le replay",
    start_recording: "Enregistrer en continu",
    stop_recording: "Arrêter l’enregistrement",
    take_screenshot: "Capture d’écran",
    open_clips_dir: "Ouvrir le dossier des shards",
    settings: "Réglages",
    quit: "Quitter",
    choose_clips_dir: "Choisir le dossier des shards",
    desktop: "Bureau",
    edited: "montage",
    presence_playing: "Clippe sur {game}",
    presence_desktop: "Sur le bureau",
    presence_recording: "Enregistre sa partie",
    presence_button: "Télécharger Krak",
    presence_today: Plural {
        one: "{count} shard capturé aujourd’hui",
        few: "{count} shards capturés aujourd’hui",
        many: "{count} shards capturés aujourd’hui",
        other: "{count} shards capturés aujourd’hui",
    },
};

const ES: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Abrir Krak",
    capture_now: "Capturar ahora",
    pause_replay: "Pausar el replay",
    resume_replay: "Reanudar el replay",
    start_recording: "Empezar a grabar",
    stop_recording: "Detener la grabación",
    take_screenshot: "Captura de pantalla",
    open_clips_dir: "Abrir la carpeta de shards",
    settings: "Ajustes",
    quit: "Salir",
    choose_clips_dir: "Elegir la carpeta de shards",
    desktop: "Escritorio",
    edited: "edición",
    presence_playing: "Haciendo clips en {game}",
    presence_desktop: "En el escritorio",
    presence_recording: "Grabando su partida",
    presence_button: "Descargar Krak",
    presence_today: Plural {
        one: "{count} shard capturado hoy",
        few: "{count} shards capturados hoy",
        many: "{count} shards capturados hoy",
        other: "{count} shards capturados hoy",
    },
};

const PT: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Abrir o Krak",
    capture_now: "Capturar agora",
    pause_replay: "Pausar o replay",
    resume_replay: "Retomar o replay",
    start_recording: "Iniciar gravação",
    stop_recording: "Parar gravação",
    take_screenshot: "Captura de tela",
    open_clips_dir: "Abrir a pasta dos shards",
    settings: "Configurações",
    quit: "Sair",
    choose_clips_dir: "Escolher a pasta dos shards",
    desktop: "Área de trabalho",
    edited: "editado",
    presence_playing: "Clipando {game}",
    presence_desktop: "Na área de trabalho",
    presence_recording: "Gravando a partida",
    presence_button: "Baixar o Krak",
    presence_today: Plural {
        one: "{count} shard capturado hoje",
        few: "{count} shards capturados hoje",
        many: "{count} shards capturados hoje",
        other: "{count} shards capturados hoje",
    },
};

const DE: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Krak öffnen",
    capture_now: "Jetzt erfassen",
    pause_replay: "Replay pausieren",
    resume_replay: "Replay fortsetzen",
    start_recording: "Aufnahme starten",
    stop_recording: "Aufnahme beenden",
    take_screenshot: "Screenshot",
    open_clips_dir: "Shard-Ordner öffnen",
    settings: "Einstellungen",
    quit: "Beenden",
    choose_clips_dir: "Shard-Ordner auswählen",
    desktop: "Desktop",
    edited: "bearbeitet",
    presence_playing: "Clippt in {game}",
    presence_desktop: "Auf dem Desktop",
    presence_recording: "Nimmt das Spiel auf",
    presence_button: "Hol dir Krak",
    presence_today: Plural {
        one: "Heute {count} Shard erfasst",
        few: "Heute {count} Shards erfasst",
        many: "Heute {count} Shards erfasst",
        other: "Heute {count} Shards erfasst",
    },
};

const IT: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Apri Krak",
    capture_now: "Cattura ora",
    pause_replay: "Metti in pausa il replay",
    resume_replay: "Riprendi il replay",
    start_recording: "Avvia registrazione",
    stop_recording: "Interrompi registrazione",
    take_screenshot: "Screenshot",
    open_clips_dir: "Apri la cartella degli shards",
    settings: "Impostazioni",
    quit: "Esci",
    choose_clips_dir: "Scegli la cartella degli shards",
    desktop: "Desktop",
    edited: "modifica",
    presence_playing: "Fa clip su {game}",
    presence_desktop: "Sul desktop",
    presence_recording: "Registra la sua partita",
    presence_button: "Scarica Krak",
    presence_today: Plural {
        one: "{count} shard catturato oggi",
        few: "{count} shards catturati oggi",
        many: "{count} shards catturati oggi",
        other: "{count} shards catturati oggi",
    },
};

const PL: Labels = Labels {
    rule: Rule::Polish,
    open_app: "Otwórz Krak",
    capture_now: "Przechwyć teraz",
    pause_replay: "Wstrzymaj powtórkę",
    resume_replay: "Wznów powtórkę",
    start_recording: "Zacznij nagrywanie",
    stop_recording: "Zatrzymaj nagrywanie",
    take_screenshot: "Zrzut ekranu",
    open_clips_dir: "Otwórz folder shardów",
    settings: "Ustawienia",
    quit: "Zakończ",
    choose_clips_dir: "Wybierz folder shardów",
    desktop: "Pulpit",
    edited: "edycja",
    presence_playing: "Nagrywa klipy w {game}",
    presence_desktop: "Na pulpicie",
    presence_recording: "Nagrywa swoją rozgrywkę",
    presence_button: "Pobierz Krak",
    presence_today: Plural {
        one: "Dziś przechwycono {count} shard",
        few: "Dziś przechwycono {count} shardy",
        many: "Dziś przechwycono {count} shardów",
        other: "Dziś przechwycono {count} sharda",
    },
};

const TR: Labels = Labels {
    rule: Rule::OneIsOne,
    open_app: "Krak’ı aç",
    capture_now: "Şimdi yakala",
    pause_replay: "Anında tekrarı duraklat",
    resume_replay: "Anında tekrarı sürdür",
    start_recording: "Kaydı başlat",
    stop_recording: "Kaydı durdur",
    take_screenshot: "Ekran görüntüsü",
    open_clips_dir: "Shard klasörünü aç",
    settings: "Ayarlar",
    quit: "Çıkış",
    choose_clips_dir: "Shard klasörünü seç",
    desktop: "Masaüstü",
    edited: "düzenleme",
    presence_playing: "Klip alıyor: {game}",
    presence_desktop: "Masaüstünde",
    presence_recording: "Oyununu kaydediyor",
    presence_button: "Krak’ı indir",
    presence_today: Plural {
        one: "Bugün {count} shard yakalandı",
        few: "Bugün {count} shard yakalandı",
        many: "Bugün {count} shard yakalandı",
        other: "Bugün {count} shard yakalandı",
    },
};

const RU: Labels = Labels {
    rule: Rule::Russian,
    open_app: "Открыть Krak",
    capture_now: "Захватить сейчас",
    pause_replay: "Приостановить повтор",
    resume_replay: "Возобновить повтор",
    start_recording: "Начать запись",
    stop_recording: "Остановить запись",
    take_screenshot: "Скриншот",
    open_clips_dir: "Открыть папку для shard",
    settings: "Настройки",
    quit: "Выход",
    choose_clips_dir: "Выбрать папку для shard",
    desktop: "Рабочий стол",
    edited: "правка",
    presence_playing: "Записывает клипы в {game}",
    presence_desktop: "На рабочем столе",
    presence_recording: "Записывает свою игру",
    presence_button: "Скачать Krak",
    presence_today: Plural {
        one: "Сегодня сохранён {count} shard",
        few: "Сегодня сохранено {count} shard",
        many: "Сегодня сохранено {count} shard",
        other: "Сегодня сохранено {count} shard",
    },
};

const JA: Labels = Labels {
    rule: Rule::Invariable,
    open_app: "Krak を開く",
    capture_now: "今すぐキャプチャ",
    pause_replay: "リプレイを一時停止",
    resume_replay: "リプレイを再開",
    start_recording: "録画を開始",
    stop_recording: "録画を停止",
    take_screenshot: "スクリーンショット",
    open_clips_dir: "shard の保存フォルダーを開く",
    settings: "設定",
    quit: "終了",
    choose_clips_dir: "shard の保存フォルダーを選択",
    desktop: "デスクトップ",
    edited: "編集",
    presence_playing: "{game} をクリップ中",
    presence_desktop: "デスクトップで待機中",
    presence_recording: "ゲームを録画中",
    presence_button: "Krak を入手",
    presence_today: Plural {
        one: "今日 {count}件の shard をキャプチャ",
        few: "今日 {count}件の shard をキャプチャ",
        many: "今日 {count}件の shard をキャプチャ",
        other: "今日 {count}件の shard をキャプチャ",
    },
};

const KO: Labels = Labels {
    rule: Rule::Invariable,
    open_app: "Krak 열기",
    capture_now: "지금 캡처",
    pause_replay: "리플레이 일시 정지",
    resume_replay: "리플레이 재개",
    start_recording: "녹화 시작",
    stop_recording: "녹화 중지",
    take_screenshot: "스크린샷",
    open_clips_dir: "shard 폴더 열기",
    settings: "설정",
    quit: "종료",
    choose_clips_dir: "shard 폴더 선택",
    desktop: "바탕 화면",
    edited: "편집",
    presence_playing: "{game} 클립 중",
    presence_desktop: "바탕 화면에서 대기 중",
    presence_recording: "게임 녹화 중",
    presence_button: "Krak 받기",
    presence_today: Plural {
        one: "오늘 shard {count}개 캡처",
        few: "오늘 shard {count}개 캡처",
        many: "오늘 shard {count}개 캡처",
        other: "오늘 shard {count}개 캡처",
    },
};

const ZH: Labels = Labels {
    rule: Rule::Invariable,
    open_app: "打开 Krak",
    capture_now: "立即捕获",
    pause_replay: "暂停回放",
    resume_replay: "继续回放",
    start_recording: "开始录制",
    stop_recording: "停止录制",
    take_screenshot: "截图",
    open_clips_dir: "打开 shard 文件夹",
    settings: "设置",
    quit: "退出",
    choose_clips_dir: "选择 shard 文件夹",
    desktop: "桌面",
    edited: "编辑",
    presence_playing: "正在捕获 {game} 的高光",
    presence_desktop: "在桌面",
    presence_recording: "正在录制游戏",
    presence_button: "获取 Krak",
    presence_today: Plural {
        one: "今天捕获了 {count} 个 shard",
        few: "今天捕获了 {count} 个 shard",
        many: "今天捕获了 {count} 个 shard",
        other: "今天捕获了 {count} 个 shard",
    },
};

/// By the code the interface uses for the language (src/lib/locales/index.ts).
static LANGUAGES: [(&str, &Labels); 12] = [
    ("en", &EN),
    ("fr", &FR),
    ("es", &ES),
    ("pt", &PT),
    ("de", &DE),
    ("it", &IT),
    ("pl", &PL),
    ("tr", &TR),
    ("ru", &RU),
    ("ja", &JA),
    ("ko", &KO),
    ("zh", &ZH),
];

/// The labels of `code` ("pt", "pt-BR", "zh_CN"…) when the backend has that language.
fn find(code: &str) -> Option<&'static Labels> {
    let base = code.split(['-', '_']).next().unwrap_or(code);
    LANGUAGES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(base))
        .map(|&(_, labels)| labels)
}

/// The configured language, else the first of the system's preferred languages that Krak
/// speaks (as the interface picks it), else English.
pub fn labels(language: Option<&str>) -> &'static Labels {
    language
        .and_then(find)
        .or_else(|| sys_locale::get_locales().find_map(|locale| find(&locale)))
        .unwrap_or(&EN)
}

/// Every edited-copy suffix, so re-editing a copy made in another language doesn't stack them.
pub fn edited_suffixes() -> impl Iterator<Item = &'static str> {
    LANGUAGES.iter().map(|(_, labels)| labels.edited)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_regional_codes_and_falls_back() {
        assert_eq!(labels(Some("fr")).quit, FR.quit);
        assert_eq!(labels(Some("pt-BR")).quit, PT.quit);
        assert_eq!(labels(Some("zh_CN")).quit, ZH.quit);
        assert!(find("xx").is_none());
    }

    #[test]
    fn edited_suffixes_are_distinct() {
        let all: Vec<_> = edited_suffixes().collect();
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(all.len(), unique.len());
    }

    #[test]
    fn plural_forms_follow_each_language() {
        let forms = |labels: &Labels, counts: &[u64]| -> Vec<&str> {
            counts
                .iter()
                .map(|&n| labels.rule.pick(&labels.presence_today, n))
                .collect()
        };
        let p = &EN.presence_today;
        assert_eq!(forms(&EN, &[0, 1, 2]), [p.other, p.one, p.other]);
        let p = &FR.presence_today;
        assert_eq!(forms(&FR, &[0, 1, 2]), [p.one, p.one, p.other]);
        let p = &PL.presence_today;
        assert_eq!(
            forms(&PL, &[1, 2, 5, 12, 22, 25]),
            [p.one, p.few, p.many, p.many, p.few, p.many]
        );
        let p = &RU.presence_today;
        assert_eq!(
            forms(&RU, &[1, 3, 5, 11, 21, 112]),
            [p.one, p.few, p.many, p.many, p.one, p.many]
        );
        assert_eq!(EN.shards_today(3), "3 shards captured today");
    }
}
