//! Translates SDL controller and keyboard events into menu actions.
//!
//! The dead-zone and key-repeat logic is plain code driven by explicit
//! timestamps, so it can be tested without a controller attached. SDL's
//! semantic controller API supplies the buttons, so numeric button IDs never
//! appear here.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use sdl2::GameControllerSubsystem;
use sdl2::controller::{Axis, Button, GameController};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;

use crate::menu::Nav;

/// Time a direction must be held before it starts repeating.
const REPEAT_DELAY_MS: u64 = 350;
const REPEAT_INTERVAL_MS: u64 = 110;
/// Paging moves a whole screen, so it repeats more slowly.
const PAGE_REPEAT_INTERVAL_MS: u64 = 280;
/// Stick deflection needed to start moving (about 40% of full travel).
const STICK_ENGAGE: i32 = 13_000;
/// Deflection below which the stick is released. Keeping this lower than
/// `STICK_ENGAGE` stops a stick resting on the threshold from chattering.
const STICK_RELEASE: i32 = 9_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Navigate(Nav),
    Launch,
    CycleSort,
}

/// Input sources in priority order: a digital press overrides the stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Digital = 0,
    Stick = 1,
}

/// Tracks held directions and decides when each should fire.
#[derive(Debug, Default)]
struct Repeater {
    held: [Option<Nav>; 2],
    active: Option<Nav>,
    next_fire: u64,
}

impl Repeater {
    /// Updates a source. Returns a direction if it should fire immediately.
    fn set(&mut self, source: Source, nav: Option<Nav>, now: u64) -> Option<Nav> {
        self.held[source as usize] = nav;
        let active = self.held.iter().flatten().next().copied();
        if active == self.active {
            return None;
        }
        self.active = active;
        self.next_fire = now + REPEAT_DELAY_MS;
        active
    }

    /// Releases `nav` from `source` only if that direction is the one held.
    fn release(&mut self, source: Source, nav: Nav, now: u64) -> Option<Nav> {
        if self.held[source as usize] == Some(nav) {
            self.set(source, None, now)
        } else {
            None
        }
    }

    /// Returns the held direction once each time its repeat is due.
    fn poll(&mut self, now: u64) -> Option<Nav> {
        let nav = self.active.filter(|_| now >= self.next_fire)?;
        let interval = match nav {
            Nav::PageUp | Nav::PageDown => PAGE_REPEAT_INTERVAL_MS,
            _ => REPEAT_INTERVAL_MS,
        };
        self.next_fire = now + interval;
        Some(nav)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Maps stick position to a direction along its dominant axis. `current`
/// supplies hysteresis: an engaged stick stays engaged until `STICK_RELEASE`.
fn stick_direction(x: i16, y: i16, current: Option<Nav>) -> Option<Nav> {
    let (x, y) = (i32::from(x), i32::from(y));
    let threshold = if current.is_some() {
        STICK_RELEASE
    } else {
        STICK_ENGAGE
    };
    if x.abs().max(y.abs()) < threshold {
        None
    } else if x.abs() > y.abs() {
        Some(if x < 0 { Nav::Left } else { Nav::Right })
    } else {
        Some(if y < 0 { Nav::Up } else { Nav::Down })
    }
}

fn button_nav(button: Button) -> Option<Nav> {
    match button {
        Button::DPadUp => Some(Nav::Up),
        Button::DPadDown => Some(Nav::Down),
        Button::DPadLeft => Some(Nav::Left),
        Button::DPadRight => Some(Nav::Right),
        Button::LeftShoulder => Some(Nav::PageUp),
        Button::RightShoulder => Some(Nav::PageDown),
        _ => None,
    }
}

fn key_nav(key: Keycode) -> Option<Nav> {
    match key {
        Keycode::Up => Some(Nav::Up),
        Keycode::Down => Some(Nav::Down),
        Keycode::Left => Some(Nav::Left),
        Keycode::Right => Some(Nav::Right),
        Keycode::PageUp => Some(Nav::PageUp),
        Keycode::PageDown => Some(Nav::PageDown),
        _ => None,
    }
}

struct Pad {
    /// Keeps the controller open; closing it stops its events.
    _controller: GameController,
    x: i16,
    y: i16,
    direction: Option<Nav>,
}

pub struct Input {
    subsystem: GameControllerSubsystem,
    pads: HashMap<u32, Pad>,
    repeater: Repeater,
}

impl Input {
    /// Opens every controller that is already connected.
    pub fn new(subsystem: GameControllerSubsystem) -> Self {
        let mut input = Self {
            subsystem,
            pads: HashMap::new(),
            repeater: Repeater::default(),
        };
        let count = input.subsystem.num_joysticks().unwrap_or(0);
        for index in 0..count {
            input.open(index);
        }
        input
    }

    fn open(&mut self, joystick_index: u32) {
        if !self.subsystem.is_game_controller(joystick_index) {
            let name = self.subsystem.name_for_index(joystick_index);
            log_warn!(
                "joystick {joystick_index} ({}) has no SDL controller mapping; ignoring it",
                name.unwrap_or_default()
            );
            return;
        }
        match self.subsystem.open(joystick_index) {
            Ok(controller) => {
                // SDL also announces controllers present at startup, so this
                // can be a second handle on one that is already open.
                if let Entry::Vacant(entry) = self.pads.entry(controller.instance_id()) {
                    log_info!("controller connected: {}", controller.name());
                    entry.insert(Pad {
                        _controller: controller,
                        x: 0,
                        y: 0,
                        direction: None,
                    });
                }
            }
            Err(e) => log_warn!("cannot open controller {joystick_index}: {e}"),
        }
    }

    pub fn connected(&self) -> usize {
        self.pads.len()
    }

    /// True while a direction is held and the loop must wake for repeats.
    pub fn is_repeating(&self) -> bool {
        self.repeater.active.is_some()
    }

    /// Handles one SDL event, appending any resulting actions.
    pub fn handle_event(&mut self, event: &Event, now: u64, actions: &mut Vec<Action>) {
        let fired =
            match *event {
                Event::ControllerDeviceAdded { which, .. } => {
                    self.open(which);
                    None
                }
                Event::ControllerDeviceRemoved { which, .. } => {
                    if self.pads.remove(&which).is_some() {
                        log_info!("controller disconnected");
                    }
                    // A vanished controller never sends its button release.
                    self.repeater.clear();
                    None
                }
                Event::ControllerButtonDown { button, .. } => match button {
                    Button::A => {
                        actions.push(Action::Launch);
                        None
                    }
                    Button::Y | Button::Back => {
                        actions.push(Action::CycleSort);
                        None
                    }
                    _ => button_nav(button)
                        .and_then(|nav| self.repeater.set(Source::Digital, Some(nav), now)),
                },
                Event::ControllerButtonUp { button, .. } => button_nav(button)
                    .and_then(|nav| self.repeater.release(Source::Digital, nav, now)),
                Event::ControllerAxisMotion {
                    which, axis, value, ..
                } => self.handle_axis(which, axis, value, now),
                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => match key {
                    Keycode::Return | Keycode::KpEnter => {
                        actions.push(Action::Launch);
                        None
                    }
                    Keycode::Tab => {
                        actions.push(Action::CycleSort);
                        None
                    }
                    _ => key_nav(key)
                        .and_then(|nav| self.repeater.set(Source::Digital, Some(nav), now)),
                },
                Event::KeyUp {
                    keycode: Some(key), ..
                } => key_nav(key).and_then(|nav| self.repeater.release(Source::Digital, nav, now)),
                _ => None,
            };
        actions.extend(fired.map(Action::Navigate));
    }

    fn handle_axis(&mut self, which: u32, axis: Axis, value: i16, now: u64) -> Option<Nav> {
        let pad = self.pads.get_mut(&which)?;
        match axis {
            Axis::LeftX => pad.x = value,
            Axis::LeftY => pad.y = value,
            _ => return None,
        }
        let direction = stick_direction(pad.x, pad.y, pad.direction);
        if direction == pad.direction {
            // Unchanged, so an idle pad cannot cancel another pad's direction.
            return None;
        }
        pad.direction = direction;
        self.repeater.set(Source::Stick, direction, now)
    }

    /// Emits any repeat that has become due.
    pub fn tick(&mut self, now: u64, actions: &mut Vec<Action>) {
        actions.extend(self.repeater.poll(now).map(Action::Navigate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_fires_once_immediately() {
        let mut repeater = Repeater::default();
        assert_eq!(
            repeater.set(Source::Digital, Some(Nav::Down), 1000),
            Some(Nav::Down)
        );
        assert_eq!(repeater.set(Source::Digital, Some(Nav::Down), 1001), None);
        assert_eq!(repeater.poll(1001), None);
    }

    #[test]
    fn repeat_waits_for_the_delay_then_keeps_its_interval() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Digital, Some(Nav::Down), 0);
        assert_eq!(repeater.poll(REPEAT_DELAY_MS - 1), None);
        assert_eq!(repeater.poll(REPEAT_DELAY_MS), Some(Nav::Down));
        assert_eq!(repeater.poll(REPEAT_DELAY_MS + 1), None);
        assert_eq!(
            repeater.poll(REPEAT_DELAY_MS + REPEAT_INTERVAL_MS),
            Some(Nav::Down)
        );
    }

    #[test]
    fn a_long_stall_produces_one_step_not_a_burst() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Digital, Some(Nav::Right), 0);
        assert!(repeater.poll(60_000).is_some());
        assert!(repeater.poll(60_000).is_none());
    }

    #[test]
    fn paging_repeats_more_slowly() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Digital, Some(Nav::PageDown), 0);
        assert!(repeater.poll(REPEAT_DELAY_MS).is_some());
        assert!(
            repeater
                .poll(REPEAT_DELAY_MS + REPEAT_INTERVAL_MS)
                .is_none()
        );
        assert!(
            repeater
                .poll(REPEAT_DELAY_MS + PAGE_REPEAT_INTERVAL_MS)
                .is_some()
        );
    }

    #[test]
    fn release_stops_repeating() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Digital, Some(Nav::Up), 0);
        repeater.release(Source::Digital, Nav::Up, 10);
        assert_eq!(repeater.poll(10_000), None);
    }

    #[test]
    fn releasing_an_older_press_keeps_the_newer_direction() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Digital, Some(Nav::Left), 0);
        repeater.set(Source::Digital, Some(Nav::Right), 10);
        assert_eq!(repeater.release(Source::Digital, Nav::Left, 20), None);
        assert!(repeater.active == Some(Nav::Right));
    }

    #[test]
    fn digital_input_overrides_and_returns_to_the_stick() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Stick, Some(Nav::Left), 0);
        assert_eq!(
            repeater.set(Source::Digital, Some(Nav::Up), 10),
            Some(Nav::Up)
        );
        assert_eq!(
            repeater.release(Source::Digital, Nav::Up, 20),
            Some(Nav::Left)
        );
    }

    #[test]
    fn clear_forgets_everything() {
        let mut repeater = Repeater::default();
        repeater.set(Source::Stick, Some(Nav::Left), 0);
        repeater.clear();
        assert_eq!(repeater.poll(10_000), None);
    }

    #[test]
    fn dead_zone_ignores_small_deflection() {
        assert_eq!(stick_direction(0, 0, None), None);
        assert_eq!(stick_direction(STICK_ENGAGE as i16 - 1, 0, None), None);
        assert_eq!(stick_direction(-8000, 8000, None), None);
    }

    #[test]
    fn stick_uses_the_dominant_axis() {
        assert_eq!(stick_direction(20000, 5000, None), Some(Nav::Right));
        assert_eq!(stick_direction(-20000, 5000, None), Some(Nav::Left));
        assert_eq!(stick_direction(5000, -20000, None), Some(Nav::Up));
        assert_eq!(stick_direction(5000, 20000, None), Some(Nav::Down));
        assert_eq!(stick_direction(i16::MIN, 0, None), Some(Nav::Left));
    }

    #[test]
    fn stick_hysteresis_prevents_chatter() {
        let middle = ((STICK_ENGAGE + STICK_RELEASE) / 2) as i16;
        assert_eq!(stick_direction(middle, 0, None), None);
        assert_eq!(
            stick_direction(middle, 0, Some(Nav::Right)),
            Some(Nav::Right)
        );
        let below = STICK_RELEASE as i16 - 1;
        assert_eq!(stick_direction(below, 0, Some(Nav::Right)), None);
    }

    #[test]
    fn shoulders_page_and_face_buttons_do_not_navigate() {
        assert_eq!(button_nav(Button::LeftShoulder), Some(Nav::PageUp));
        assert_eq!(button_nav(Button::RightShoulder), Some(Nav::PageDown));
        assert_eq!(button_nav(Button::A), None);
        assert_eq!(button_nav(Button::Y), None);
    }
}
