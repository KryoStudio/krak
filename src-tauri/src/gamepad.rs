//! Controller shortcut: holding a combination of buttons on a controller saves the replay,
//! like the clip hotkey. With `controllerRumble`, a short buzz says the shard is kept and two
//! say it failed: felt in the hands, so it also works over a game in exclusive fullscreen,
//! where no notification can show.
//!
//! Controllers are read through XInput: Xbox controllers, and the others through a tool that
//! presents them as one (DS4Windows, DualSenseX...). Reading never takes the controller from
//! the game: both see every press. One background thread does it, only while a combination is
//! set: every 20 ms for the connected controllers, every 2 s for the empty slots (XInput is
//! slow to answer for a controller that is not there). Without a combination it sleeps.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use windows::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK, XINPUT_GAMEPAD_DPAD_DOWN,
    XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP,
    XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_LEFT_THUMB, XINPUT_GAMEPAD_RIGHT_SHOULDER,
    XINPUT_GAMEPAD_RIGHT_THUMB, XINPUT_GAMEPAD_START, XINPUT_GAMEPAD_TRIGGER_THRESHOLD,
    XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y, XINPUT_STATE, XINPUT_VIBRATION, XInputGetState,
    XInputSetState,
};

use crate::replay::{self, Press};
use crate::settings::Settings;
use crate::state::{AppState, lock};

pub const NO_CONTROLLER: &str = "no_controller";
/// Nothing was pressed while choosing the buttons.
pub const COMBO_TIMEOUT: &str = "combo_timeout";
/// Only one button was held: a combination needs two or more.
pub const COMBO_SHORT: &str = "combo_short";

/// XInput's controller slots.
const SLOTS: u32 = 4;
const POLL: Duration = Duration::from_millis(20);
const RESCAN: Duration = Duration::from_secs(2);
const PULSE: Duration = Duration::from_millis(130);
const PAUSE: Duration = Duration::from_millis(110);
/// Motor speeds of a buzz (of 65 535): felt, not a jolt.
const STRENGTH: (u16, u16) = (26_000, 34_000);
/// How long choosing the buttons waits for them.
const CAPTURE_FOR: Duration = Duration::from_secs(10);
const CAPTURE_POLL: Duration = Duration::from_millis(10);
/// The triggers count as buttons once pressed past XInput's threshold.
const LT: u32 = 1 << 16;
const RT: u32 = 1 << 17;

/// Every button by name, in the order a combination is written.
const BUTTONS: [(&str, u32); 16] = [
    ("LB", XINPUT_GAMEPAD_LEFT_SHOULDER.0 as u32),
    ("RB", XINPUT_GAMEPAD_RIGHT_SHOULDER.0 as u32),
    ("LT", LT),
    ("RT", RT),
    ("LS", XINPUT_GAMEPAD_LEFT_THUMB.0 as u32),
    ("RS", XINPUT_GAMEPAD_RIGHT_THUMB.0 as u32),
    ("View", XINPUT_GAMEPAD_BACK.0 as u32),
    ("Menu", XINPUT_GAMEPAD_START.0 as u32),
    ("A", XINPUT_GAMEPAD_A.0 as u32),
    ("B", XINPUT_GAMEPAD_B.0 as u32),
    ("X", XINPUT_GAMEPAD_X.0 as u32),
    ("Y", XINPUT_GAMEPAD_Y.0 as u32),
    ("Up", XINPUT_GAMEPAD_DPAD_UP.0 as u32),
    ("Down", XINPUT_GAMEPAD_DPAD_DOWN.0 as u32),
    ("Left", XINPUT_GAMEPAD_DPAD_LEFT.0 as u32),
    ("Right", XINPUT_GAMEPAD_DPAD_RIGHT.0 as u32),
];

/// Two buttons or more held together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo(u32);

impl Combo {
    /// "LB+RB" (any case, spaces allowed): known buttons only, two at least.
    pub fn parse(text: &str) -> Option<Self> {
        let mut bits = 0;
        for name in text.split('+') {
            let name = name.trim();
            let (_, bit) = BUTTONS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?;
            bits |= bit;
        }
        Self::of(bits)
    }

    fn of(bits: u32) -> Option<Self> {
        (bits.count_ones() >= 2).then_some(Self(bits))
    }

    fn held(self, buttons: u32) -> bool {
        buttons & self.0 == self.0
    }
}

impl fmt::Display for Combo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = BUTTONS
            .iter()
            .filter(|(_, bit)| self.0 & bit != 0)
            .map(|(name, _)| *name)
            .collect();
        f.write_str(&names.join("+"))
    }
}

/// The buttons held on the controller in `slot`, `None` when there is none.
fn read(slot: u32) -> Option<u32> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: an out-parameter we own.
    if unsafe { XInputGetState(slot, &mut state) } != 0 {
        return None;
    }
    let pad = state.Gamepad;
    let threshold = XINPUT_GAMEPAD_TRIGGER_THRESHOLD.0;
    let mut buttons = u32::from(pad.wButtons.0);
    if u16::from(pad.bLeftTrigger) > threshold {
        buttons |= LT;
    }
    if u16::from(pad.bRightTrigger) > threshold {
        buttons |= RT;
    }
    Some(buttons)
}

fn vibrate(slot: u32, on: bool) {
    let (left, right) = if on { STRENGTH } else { (0, 0) };
    let vibration = XINPUT_VIBRATION {
        wLeftMotorSpeed: left,
        wRightMotorSpeed: right,
    };
    // SAFETY: an in-parameter we own; a missing controller just fails.
    let _ = unsafe { XInputSetState(slot, &vibration) };
}

/// Managed state: the link to the controller thread.
pub struct Gamepad {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    woken: Mutex<bool>,
    wake: Condvar,
    /// The settings are choosing new buttons: the shortcut does not fire meanwhile.
    capturing: AtomicBool,
}

impl Shared {
    fn notify(&self) {
        *lock(&self.woken) = true;
        self.wake.notify_all();
    }

    /// Sleeps until woken, or for `timeout`.
    fn wait(&self, timeout: Option<Duration>) {
        let woken = lock(&self.woken);
        let mut woken = match timeout {
            Some(timeout) => {
                self.wake
                    .wait_timeout_while(woken, timeout, |woken| !*woken)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
            None => self
                .wake
                .wait_while(woken, |woken| !*woken)
                .unwrap_or_else(|e| e.into_inner()),
        };
        *woken = false;
    }
}

/// Starts the controller thread (after `replay::init`: it saves through it).
pub fn init(app: &AppHandle) {
    let shared = Arc::new(Shared::default());
    app.manage(Gamepad {
        shared: shared.clone(),
    });
    let app = app.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("krak-gamepad".into())
        .spawn(move || run(&app, &shared))
    {
        eprintln!("krak: cannot start the controller thread: {err}");
    }
}

/// After a settings update: starts or stops watching the controllers.
pub fn settings_changed(app: &AppHandle, old: &Settings, new: &Settings) {
    if old.controller_hotkey != new.controller_hotkey
        && let Some(gamepad) = app.try_state::<Gamepad>()
    {
        gamepad.shared.notify();
    }
}

/// Settings: waits (up to 10 s) for buttons held together then released, and returns them as
/// a combination.
pub fn capture(app: &AppHandle) -> Result<String, &'static str> {
    let gamepad = app.state::<Gamepad>();
    gamepad.shared.capturing.store(true, Ordering::SeqCst);
    let result = capture_combo();
    gamepad.shared.capturing.store(false, Ordering::SeqCst);
    result
}

fn capture_combo() -> Result<String, &'static str> {
    let deadline = Instant::now() + CAPTURE_FOR;
    let mut connected = false;
    // The most buttons held at once (several controllers count as one).
    let mut widest = 0u32;
    loop {
        let mut held = 0;
        for slot in 0..SLOTS {
            if let Some(buttons) = read(slot) {
                connected = true;
                held |= buttons;
            }
        }
        if !connected {
            return Err(NO_CONTROLLER);
        }
        if held.count_ones() > widest.count_ones() {
            widest = held;
        }
        if held == 0 && widest != 0 {
            return Combo::of(widest)
                .map(|combo| combo.to_string())
                .ok_or(COMBO_SHORT);
        }
        if Instant::now() >= deadline {
            return Err(COMBO_TIMEOUT);
        }
        std::thread::sleep(CAPTURE_POLL);
    }
}

/// One controller slot, as the thread last saw it.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    connected: bool,
    /// An empty slot is looked at again from then.
    next_look: Option<Instant>,
    /// The combination was held at the last look (a press fires once, until released).
    held: bool,
}

/// Short buzzes on one controller, one after the other.
struct Buzz {
    slot: u32,
    /// Buzzes left, the current one included.
    pulses: u32,
    on: bool,
    next: Instant,
}

impl Buzz {
    fn start(slot: u32, pulses: u32, now: Instant) -> Self {
        vibrate(slot, true);
        Self {
            slot,
            pulses,
            on: true,
            next: now + PULSE,
        }
    }

    /// Goes on when due; `false` once over (the motors stopped).
    fn step(&mut self, now: Instant) -> bool {
        if now < self.next {
            return true;
        }
        if self.on {
            vibrate(self.slot, false);
            self.on = false;
            self.pulses -= 1;
            if self.pulses == 0 {
                return false;
            }
            self.next = now + PAUSE;
        } else {
            vibrate(self.slot, true);
            self.on = true;
            self.next = now + PULSE;
        }
        true
    }
}

impl Drop for Buzz {
    fn drop(&mut self) {
        if self.on {
            vibrate(self.slot, false);
        }
    }
}

fn run(app: &AppHandle, shared: &Shared) {
    let mut slots = [Slot::default(); SLOTS as usize];
    let mut buzz: Option<Buzz> = None;
    loop {
        let settings = app.state::<AppState>().settings();
        let now = Instant::now();
        if buzz.as_mut().is_some_and(|b| !b.step(now)) {
            buzz = None;
        }
        let Some(combo) = Combo::parse(&settings.controller_hotkey) else {
            // Nothing to watch: forget the controllers and sleep (once a buzz is over).
            slots = Default::default();
            shared.wait(buzz.as_ref().map(|b| b.next.saturating_duration_since(now)));
            continue;
        };
        let mut connected = false;
        for (slot, seen) in (0..SLOTS).zip(slots.iter_mut()) {
            if !seen.connected && seen.next_look.is_some_and(|at| now < at) {
                continue;
            }
            let Some(buttons) = read(slot) else {
                *seen = Slot {
                    next_look: Some(now + RESCAN),
                    ..Slot::default()
                };
                continue;
            };
            connected = true;
            let held = combo.held(buttons);
            if held && !seen.held && !shared.capturing.load(Ordering::SeqCst) {
                let pulses = match replay::request_save(app) {
                    Press::Saving => 1,
                    Press::Failed => 2,
                    Press::Ignored => 0,
                };
                if settings.controller_rumble && pulses > 0 {
                    // The buzz going on stops before the next one starts (same motors maybe).
                    drop(buzz.take());
                    buzz = Some(Buzz::start(slot, pulses, now));
                }
            }
            *seen = Slot {
                connected: true,
                next_look: None,
                held,
            };
        }
        let mut timeout = if connected { POLL } else { RESCAN };
        if let Some(buzz) = &buzz {
            timeout = timeout.min(buzz.next.saturating_duration_since(now));
        }
        shared.wait(Some(timeout));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_combinations() {
        let combo = Combo::parse(" rb + LB ").expect("two buttons");
        assert_eq!(combo.to_string(), "LB+RB");
        assert_eq!(
            Combo::parse("LS+RS+View").map(|c| c.to_string()).as_deref(),
            Some("LS+RS+View")
        );
        assert_eq!(Combo::parse("LT+RT+Down").unwrap().0, LT | RT | 2);
        // One button, an unknown one, nothing: not a combination.
        for bad in ["A", "A+A", "LB+Nope", "", "+"] {
            assert_eq!(Combo::parse(bad), None, "{bad}");
        }
        // Every name reads back as itself, and no two share a bit.
        let all = BUTTONS.iter().fold(0, |bits, (_, bit)| {
            assert_eq!(bits & bit, 0);
            bits | bit
        });
        assert_eq!(all.count_ones(), 16);
        for (name, _) in BUTTONS {
            let pair = format!("{name}+{}", if name == "A" { "B" } else { "A" });
            assert!(Combo::parse(&pair).is_some(), "{name}");
        }
    }

    #[test]
    fn fires_only_with_every_button_of_the_combination() {
        let combo = Combo::parse("LB+RB").unwrap();
        let (lb, rb, a) = (BUTTONS[0].1, BUTTONS[1].1, BUTTONS[8].1);
        assert!(combo.held(lb | rb));
        assert!(combo.held(lb | rb | a), "other buttons may be held too");
        assert!(!combo.held(lb | a));
        assert!(!combo.held(0));
    }

    /// The controllers plugged in now, what they hold, and a buzz on the first one.
    #[test]
    #[ignore = "reads the real controllers (and buzzes the first one); run with --ignored --nocapture"]
    fn controllers_live() {
        let connected: Vec<(u32, u32)> = (0..SLOTS)
            .filter_map(|slot| read(slot).map(|buttons| (slot, buttons)))
            .collect();
        println!("controllers: {connected:?}");
        if let Some(&(slot, _)) = connected.first() {
            let mut buzz = Buzz::start(slot, 2, Instant::now());
            while buzz.step(Instant::now()) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
