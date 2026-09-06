use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, OnceLock},
};

use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{
            ConnectionExt as _, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT,
            KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    CURRENT_TIME,
};

use crate::shared_input::{InputCommand, MouseButton};

static BACKEND: OnceLock<Mutex<Option<X11Input>>> = OnceLock::new();

struct X11Input {
    connection: RustConnection,
    root: u32,
    pressed_keys: HashMap<u16, u8>,
    pressed_buttons: HashSet<u8>,
}

fn validate_session(
    session: Option<&str>,
    wayland: Option<&str>,
    display: Option<&str>,
) -> Result<(), String> {
    if session.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
        || wayland.is_some_and(|value| !value.is_empty())
    {
        return Err("Wayland input injection is not supported yet. Log out and select 'Ubuntu on Xorg' at the login screen, then restart MyKVM.".into());
    }
    if display.map_or(true, |value| value.is_empty()) {
        return Err("No X11 DISPLAY is available. Start MyKVM inside the logged-in Ubuntu Xorg desktop session.".into());
    }
    Ok(())
}

fn with_backend<T>(
    operation: impl FnOnce(&mut X11Input) -> Result<T, String>,
) -> Result<T, String> {
    validate_session(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    )?;
    let mut backend = BACKEND
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Linux input backend lock is poisoned".to_string())?;
    if backend.is_none() {
        *backend = Some(X11Input::connect()?);
    }
    let input = backend.as_mut().expect("X11 backend initialized");
    let result = operation(input);
    if result.is_err() && input.check_connection().is_err() {
        // A disconnected display must be reopened on the next attempt. Keep
        // held-key state when only an individual command was unsupported.
        *backend = None;
    }
    result
}

pub fn ready() -> Result<(), String> {
    with_backend(|backend| backend.check_connection())
}

pub fn inject(command: &InputCommand) -> Result<(), String> {
    with_backend(|backend| backend.inject(command))
}

pub fn release_all() {
    // Stopping sharing must not create a new display connection.
    if let Some(backend) = BACKEND.get() {
        if let Ok(mut backend) = backend.lock() {
            if let Some(backend) = backend.as_mut() {
                if let Err(error) = backend.inject(&InputCommand::ReleaseAll) {
                    log::warn!("release Linux shared input: {error}");
                }
            }
        }
    }
}

fn x11_error(error: impl std::fmt::Display) -> String {
    format!("X11 input injection failed: {error}")
}

impl X11Input {
    fn check_connection(&self) -> Result<(), String> {
        self.connection
            .get_input_focus()
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        Ok(())
    }

    fn connect() -> Result<Self, String> {
        let (connection, screen) = x11rb::connect(None)
            .map_err(|error| format!("Cannot connect to the X11 desktop: {error}. Check DISPLAY and XAUTHORITY and run MyKVM as the desktop user."))?;
        connection
            .xtest_get_version(2, 2)
            .map_err(x11_error)?
            .reply()
            .map_err(|error| format!("X11 XTEST extension is unavailable: {error}"))?;
        let root = connection.setup().roots[screen].root;
        Ok(Self {
            connection,
            root,
            pressed_keys: HashMap::new(),
            pressed_buttons: HashSet::new(),
        })
    }

    fn event(&self, event_type: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        self.connection
            .xtest_fake_input(event_type, detail, CURRENT_TIME, self.root, x, y, 0)
            .map_err(x11_error)?
            .check()
            .map_err(x11_error)
    }

    fn move_pointer(&self, x: i32, y: i32) -> Result<(), String> {
        let x =
            i16::try_from(x).map_err(|_| "Mouse X coordinate exceeds the X11 range".to_string())?;
        let y =
            i16::try_from(y).map_err(|_| "Mouse Y coordinate exceeds the X11 range".to_string())?;
        self.event(MOTION_NOTIFY_EVENT, 0, x, y)
    }

    fn keycode(&self, vk: u16) -> Result<u8, String> {
        let keysym = windows_vk_to_keysym(vk)
            .ok_or_else(|| format!("Unsupported shared key code: {vk:#06x}"))?;
        let setup = self.connection.setup();
        let count = setup
            .max_keycode
            .checked_sub(setup.min_keycode)
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| "Invalid X11 keyboard mapping range".to_string())?;
        // Read the current layout, and remember the keycode until key-up so a
        // layout change while holding a key cannot leave that key pressed.
        let mapping = self
            .connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(x11_error)?
            .reply()
            .map_err(x11_error)?;
        find_keycode(
            setup.min_keycode,
            mapping.keysyms_per_keycode,
            &mapping.keysyms,
            keysym,
        )
        .ok_or_else(|| format!("The X11 keyboard layout has no key for {vk:#06x}"))
    }

    fn inject(&mut self, command: &InputCommand) -> Result<(), String> {
        match *command {
            InputCommand::MouseMove { x, y, .. } => self.move_pointer(x, y)?,
            InputCommand::MouseButton { button, down, x, y } => {
                self.move_pointer(x, y)?;
                let button = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                };
                self.event(
                    if down {
                        BUTTON_PRESS_EVENT
                    } else {
                        BUTTON_RELEASE_EVENT
                    },
                    button,
                    0,
                    0,
                )?;
                if down {
                    self.pressed_buttons.insert(button);
                } else {
                    self.pressed_buttons.remove(&button);
                }
            }
            InputCommand::Scroll { delta_x, delta_y } => {
                for (button, count) in scroll_buttons(delta_x, delta_y) {
                    for _ in 0..count {
                        self.event(BUTTON_PRESS_EVENT, button, 0, 0)?;
                        self.event(BUTTON_RELEASE_EVENT, button, 0, 0)?;
                    }
                }
            }
            InputCommand::Key { key_code, down } => {
                let key = match self.pressed_keys.get(&key_code) {
                    Some(key) => *key,
                    None => self.keycode(key_code)?,
                };
                self.event(
                    if down {
                        KEY_PRESS_EVENT
                    } else {
                        KEY_RELEASE_EVENT
                    },
                    key,
                    0,
                    0,
                )?;
                if down {
                    self.pressed_keys.insert(key_code, key);
                } else {
                    self.pressed_keys.remove(&key_code);
                }
            }
            InputCommand::ReleaseAll => {
                for key in self.pressed_keys.values() {
                    self.event(KEY_RELEASE_EVENT, *key, 0, 0)?;
                }
                for button in &self.pressed_buttons {
                    self.event(BUTTON_RELEASE_EVENT, *button, 0, 0)?;
                }
                self.pressed_keys.clear();
                self.pressed_buttons.clear();
            }
            InputCommand::SecureAttention => {
                return Err("Secure attention is only supported on Windows".into())
            }
        }
        self.connection.flush().map_err(x11_error)
    }
}

fn find_keycode(first: u8, per_keycode: u8, mapping: &[u32], keysym: u32) -> Option<u8> {
    if per_keycode == 0 {
        return None;
    }
    mapping
        .chunks(usize::from(per_keycode))
        .position(|symbols| symbols.contains(&keysym))
        .and_then(|index| u8::try_from(index).ok())
        .and_then(|index| first.checked_add(index))
}

fn scroll_buttons(delta_x: i32, delta_y: i32) -> [(u8, u32); 2] {
    // Protocol units are wheel steps: positive Y is up, positive X is right.
    [
        (
            if delta_x < 0 { 6 } else { 7 },
            delta_x.unsigned_abs().min(120),
        ),
        (
            if delta_y > 0 { 4 } else { 5 },
            delta_y.unsigned_abs().min(120),
        ),
    ]
}

fn windows_vk_to_keysym(vk: u16) -> Option<u32> {
    Some(match vk {
        0x30..=0x39 => u32::from(vk),
        0x41..=0x5A => u32::from(vk + 0x20),
        0x60..=0x69 => 0xffb0 + u32::from(vk - 0x60),
        0x70..=0x87 => 0xffbe + u32::from(vk - 0x70),
        0x08 => 0xff08,
        0x09 => 0xff09,
        0x0C => 0xff0b,
        0x0D => 0xff0d,
        0x10 | 0xA0 => 0xffe1,
        0xA1 => 0xffe2,
        0x11 | 0xA2 => 0xffe3,
        0xA3 => 0xffe4,
        0x12 | 0xA4 => 0xffe9,
        0xA5 => 0xffea,
        0x13 => 0xff13,
        0x14 => 0xffe5,
        0x1B => 0xff1b,
        0x20 => 0x20,
        0x21 => 0xff55,
        0x22 => 0xff56,
        0x23 => 0xff57,
        0x24 => 0xff50,
        0x25 => 0xff51,
        0x26 => 0xff52,
        0x27 => 0xff53,
        0x28 => 0xff54,
        0x2C => 0xff61,
        0x2D => 0xff63,
        0x2E => 0xffff,
        0x5B => 0xffeb,
        0x5C => 0xffec,
        0x5D => 0xff67,
        0x6A => 0xffaa,
        0x6B => 0xffab,
        0x6C => 0xffac,
        0x6D => 0xffad,
        0x6E => 0xffae,
        0x6F => 0xffaf,
        0x90 => 0xff7f,
        0x91 => 0xff14,
        0xBA => 0x3b,
        0xBB => 0x3d,
        0xBC => 0x2c,
        0xBD => 0x2d,
        0xBE => 0x2e,
        0xBF => 0x2f,
        0xC0 => 0x60,
        0xDB => 0x5b,
        0xDC | 0xE2 => 0x5c,
        0xDD => 0x5d,
        0xDE => 0x27,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_is_rejected_even_when_xwayland_has_a_display() {
        assert!(validate_session(Some("wayland"), Some("wayland-0"), Some(":0")).is_err());
        assert!(validate_session(None, Some("wayland-0"), Some(":0")).is_err());
        assert!(validate_session(Some("x11"), None, Some(":0")).is_ok());
        assert!(validate_session(Some("x11"), None, None).is_err());
    }

    #[test]
    fn protocol_keys_map_to_x11_symbols() {
        assert_eq!(windows_vk_to_keysym(0x41), Some(u32::from(b'a')));
        assert_eq!(windows_vk_to_keysym(0x31), Some(u32::from(b'1')));
        assert_eq!(windows_vk_to_keysym(0x10), Some(0xffe1));
        assert_eq!(windows_vk_to_keysym(0xA1), Some(0xffe2));
        assert_eq!(windows_vk_to_keysym(0x11), Some(0xffe3));
        assert_eq!(windows_vk_to_keysym(0x5B), Some(0xffeb));
        assert_eq!(windows_vk_to_keysym(0x25), Some(0xff51));
        assert_eq!(windows_vk_to_keysym(0x70), Some(0xffbe));
        assert_eq!(windows_vk_to_keysym(0x7B), Some(0xffc9));
        assert_eq!(windows_vk_to_keysym(0xBA), Some(u32::from(b';')));
        assert_eq!(windows_vk_to_keysym(0xffff), None);
    }

    #[test]
    fn scroll_matches_protocol_direction_and_bounds_large_deltas() {
        assert_eq!(scroll_buttons(2, -3), [(7, 2), (5, 3)]);
        assert_eq!(scroll_buttons(-1, 1), [(6, 1), (4, 1)]);
        assert_eq!(scroll_buttons(0, 0), [(7, 0), (5, 0)]);
        assert_eq!(scroll_buttons(i32::MIN, i32::MAX), [(6, 120), (4, 120)]);
    }

    #[test]
    fn key_lookup_handles_nonstandard_server_keycodes() {
        let mapping = [0, 0, u32::from(b'a'), u32::from(b'A'), 0xffe1, 0];
        assert_eq!(find_keycode(20, 2, &mapping, u32::from(b'a')), Some(21));
        assert_eq!(find_keycode(20, 2, &mapping, 0xffe1), Some(22));
        assert_eq!(find_keycode(20, 2, &mapping, 0xffeb), None);
        assert_eq!(find_keycode(20, 0, &mapping, 0xffe1), None);
    }
}
