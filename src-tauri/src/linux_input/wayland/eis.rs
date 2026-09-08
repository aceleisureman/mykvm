//! EIS protocol emission, with device-scoped held input state. Never send to a
//! paused/removed device, repeat key-downs, or release on a different device.

use std::collections::{HashMap, HashSet};

use reis::{
    ei,
    event::{Connection, Device, DeviceCapability, EiEvent, Seat},
};

use super::{
    geometry::{self, Monitor, PointerMapping, Rect, Region},
    keys,
};
use crate::shared_input::{InputCommand, MouseButton};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventEffect {
    None,
    // Even an already-resumed replacement must not inherit queued input from
    // the removed keyboard. The worker applies this barrier before readiness.
    ResetInput,
}

pub(super) struct EisInput {
    connection: Connection,
    seat: Option<Seat>,
    active: Vec<Device>,
    // Only devices that have actually resumed are tracked here. A new device
    // starts paused; it must not suspend an otherwise usable session.
    paused: HashSet<Device>,
    // Snapshot capabilities before removal: reis removes the interfaces before
    // delivering DeviceRemoved, so inspecting that event's interfaces is late.
    keyboard_only_devices: HashSet<Device>,
    waiting_for_keyboard: bool,
    monitors: Vec<Monitor>,
    pointers: Vec<(PointerMapping, Device)>,
    pointer_device: Option<Device>,
    keys: HashMap<u32, Device>,
    buttons: HashMap<u32, Device>,
    controller: Option<u64>,
    sequence: u32,
}

impl EisInput {
    pub fn new(connection: Connection, monitors: Vec<Monitor>) -> Self {
        Self {
            connection,
            seat: None,
            active: Vec::new(),
            paused: HashSet::new(),
            keyboard_only_devices: HashSet::new(),
            waiting_for_keyboard: false,
            monitors,
            pointers: Vec::new(),
            pointer_device: None,
            keys: HashMap::new(),
            buttons: HashMap::new(),
            controller: None,
            sequence: 0,
        }
    }

    pub fn event(&mut self, event: EiEvent) -> Result<EventEffect, String> {
        match event {
            EiEvent::SeatAdded(event) if self.seat.is_none() => {
                log::info!("Wayland EIS seat added: {:?}", event.seat.name());
                event.seat.bind_capabilities(
                    DeviceCapability::Keyboard
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                self.seat = Some(event.seat);
            }
            EiEvent::DeviceAdded(event) => {
                if self.seat.as_ref() == Some(event.device.seat())
                    && is_keyboard_only(&event.device)
                {
                    self.keyboard_only_devices.insert(event.device.clone());
                }
                log::info!(
                    "Wayland EIS device added: name={:?}, keyboard={}, absolute_pointer={}, regions={}",
                    event.device.name(),
                    event.device.interface::<ei::Keyboard>().is_some(),
                    event.device.interface::<ei::PointerAbsolute>().is_some(),
                    event.device.regions().len(),
                );
            }
            EiEvent::DeviceResumed(event) => {
                log::info!(
                    "Wayland EIS device resumed: name={:?}, serial={}",
                    event.device.name(),
                    event.serial,
                );
                if self.seat.as_ref() == Some(event.device.seat())
                    && !self.active.contains(&event.device)
                {
                    self.paused.remove(&event.device);
                    self.sequence = self.sequence.wrapping_add(1).max(1);
                    event
                        .device
                        .device()
                        .start_emulating(self.connection.serial(), self.sequence);
                    if self.waiting_for_keyboard
                        && self.keyboard_only_devices.contains(&event.device)
                    {
                        self.waiting_for_keyboard = false;
                        log::info!("Wayland EIS replacement keyboard resumed within the existing authorization");
                    }
                    self.active.push(event.device);
                }
            }
            EiEvent::DevicePaused(event) => {
                log::info!(
                    "Wayland EIS device paused: name={:?}, serial={}",
                    event.device.name(),
                    event.serial,
                );
                if self.active.contains(&event.device) {
                    self.paused.insert(event.device.clone());
                    self.forget_device(&event.device);
                    // ei_device.paused resets this device's logical state and
                    // implicitly stops emulation. Never send releases or stop
                    // to it; release held input on the remaining active devices.
                    self.reset_active_input();
                }
            }
            EiEvent::DeviceRemoved(event) => {
                log::info!(
                    "Wayland EIS device removed: name={:?}, serial={}",
                    event.device.name(),
                    self.connection.serial(),
                );
                let keyboard_only = self.keyboard_only_devices.remove(&event.device);
                let was_paused = self.paused.remove(&event.device);
                let was_used = self.active.contains(&event.device) || was_paused;
                self.forget_device(&event.device);
                if was_used && keyboard_only {
                    // GNOME can recreate a virtual keyboard (e.g. a keymap
                    // change around lock/unlock) without revoking the portal
                    // session. Retain only this already-authorized connection;
                    // never reopen the portal or reuse the removed device.
                    self.reset_active_input();
                    self.waiting_for_keyboard = self.device::<ei::Keyboard>().is_err();
                    log::info!("Wayland EIS keyboard removed; discarding previous input and checking for a resumed keyboard on the same authorized seat");
                    return Ok(EventEffect::ResetInput);
                }
                if was_used {
                    // Pointer/mixed-device removal can change native monitor
                    // geometry. It still fails closed, even after a pause.
                    return Err(format!("A Wayland input device was removed ({:?}). Stop and start sharing to authorize again; if the display layout changed, restart MyKVM to refresh monitor geometry first.", event.device.name()));
                }
            }
            EiEvent::SeatRemoved(event) if self.seat.as_ref() == Some(&event.seat) => {
                self.invalidated();
                return Err("The Wayland input seat was removed. Restart input sharing.".into());
            }
            EiEvent::Disconnected(event) => {
                self.invalidated();
                return Err(format!(
                    "Wayland EIS disconnected ({:?}): {}. Restart input sharing.",
                    event.reason,
                    event
                        .explanation
                        .as_deref()
                        .unwrap_or("desktop session ended")
                ));
            }
            _ => {}
        }
        Ok(EventEffect::None)
    }

    fn reset_active_input(&mut self) {
        self.release_all();
        self.pointers.clear();
        self.pointer_device = None;
        self.controller = None;
    }

    fn forget_device(&mut self, device: &Device) {
        self.active.retain(|active| active != device);
        self.keys.retain(|_, held| held != device);
        self.buttons.retain(|_, held| held != device);
        self.pointers.retain(|(_, pointer)| pointer != device);
        if self.pointer_device.as_ref() == Some(device) {
            self.pointer_device = None;
        }
    }

    pub fn is_paused(&self) -> bool {
        !self.paused.is_empty() || self.waiting_for_keyboard
    }

    pub fn invalidated(&mut self) {
        self.active.clear();
        self.paused.clear();
        self.keyboard_only_devices.clear();
        self.waiting_for_keyboard = false;
        self.pointers.clear();
        self.pointer_device = None;
        self.keys.clear();
        self.buttons.clear();
        self.controller = None;
    }

    pub fn prepare(&mut self) -> Result<(), String> {
        if self.is_paused() {
            return Err("Wayland input is temporarily unavailable; waiting for paused devices or a replacement keyboard within the existing authorization.".into());
        }
        self.device::<ei::Keyboard>()?;
        self.device::<ei::Button>()?;
        self.device::<ei::Scroll>()?;
        let mut regions = Vec::new();
        let mut devices = Vec::new();
        for device in &self.active {
            if device.device().is_alive() && device.interface::<ei::PointerAbsolute>().is_some() {
                for region in device.regions() {
                    regions.push(Region {
                        // EIS regions are already in logical pixels. `scale`
                        // describes physical density, not another multiplier.
                        bounds: Rect {
                            x: f64::from(region.x),
                            y: f64::from(region.y),
                            width: f64::from(region.width),
                            height: f64::from(region.height),
                        },
                        mapping_id: region.mapping_id.clone().filter(|id| !id.is_empty()),
                    });
                    devices.push(device.clone());
                }
            }
        }
        self.pointers = geometry::bind_regions(&self.monitors, &regions)?
            .into_iter()
            .map(|mapping| {
                let device = devices[mapping.region_index].clone();
                (mapping, device)
            })
            .collect();
        Ok(())
    }

    fn device<T: ei::Interface>(&self) -> Result<Device, String> {
        self.active
            .iter()
            .find(|device| device.device().is_alive() && device.interface::<T>().is_some())
            .cloned()
            .ok_or_else(|| format!("No active Wayland EIS {} device.", T::NAME))
    }

    fn pointer_interface_device<T: ei::Interface>(&self) -> Result<Device, String> {
        if let Some(device) = &self.pointer_device {
            if self.active.contains(device)
                && device.device().is_alive()
                && device.interface::<T>().is_some()
            {
                return Ok(device.clone());
            }
        }
        self.device::<T>()
    }

    fn frame(&self, device: &Device) {
        device
            .device()
            .frame(self.connection.serial(), monotonic_micros());
    }

    fn motion(&mut self, x: i32, y: i32) -> Result<(), String> {
        let (mapping, device) = self.pointers.iter()
            .find(|(mapping, _)| mapping.native.contains(f64::from(x), f64::from(y)))
            .ok_or_else(|| "Pointer coordinate does not match an authorized Wayland monitor. Restart MyKVM after changing monitor layout.".to_string())?;
        if !self.active.contains(device) || !device.device().is_alive() {
            return Err("Wayland pointer device is no longer active.".into());
        }
        let pointer = device
            .interface::<ei::PointerAbsolute>()
            .ok_or("Wayland absolute pointer is unavailable.")?;
        let (x, y) = mapping.position(x, y);
        pointer.motion_absolute(x, y);
        self.frame(device);
        self.pointer_device = Some(device.clone());
        Ok(())
    }

    pub fn inject(&mut self, connection: u64, command: &InputCommand) -> Result<(), String> {
        if self.is_paused() {
            return Err("Wayland input is temporarily paused by the desktop.".into());
        }
        if self.controller != Some(connection) {
            self.release_all();
            self.controller = Some(connection);
        }
        match *command {
            InputCommand::MouseMove { x, y, .. } => {
                // Button state is retained by the compositor during a drag;
                // repeating a press for drag_button is an EIS protocol error.
                self.motion(x, y)?;
            }
            InputCommand::MouseButton { button, down, x, y } => {
                self.motion(x, y)?;
                let code = match button {
                    MouseButton::Left => 272,
                    MouseButton::Right => 273,
                    MouseButton::Middle => 274,
                };
                if down && !self.buttons.contains_key(&code) {
                    let device = self.pointer_interface_device::<ei::Button>()?;
                    device
                        .interface::<ei::Button>()
                        .ok_or("Wayland button device disappeared.")?
                        .button(code, ei::button::ButtonState::Press);
                    self.frame(&device);
                    self.buttons.insert(code, device);
                } else if !down {
                    if let Some(device) = self.buttons.remove(&code) {
                        if self.active.contains(&device) && device.device().is_alive() {
                            if let Some(button) = device.interface::<ei::Button>() {
                                button.button(code, ei::button::ButtonState::Released);
                                self.frame(&device);
                            }
                        }
                    }
                }
            }
            InputCommand::Key { key_code, down } => {
                let code = keys::evdev_key(key_code)
                    .ok_or_else(|| format!("Unsupported shared key code: {key_code:#06x}"))?;
                if down && !self.keys.contains_key(&code) {
                    let device = self.device::<ei::Keyboard>()?;
                    device
                        .interface::<ei::Keyboard>()
                        .ok_or("Wayland keyboard device disappeared.")?
                        .key(code, ei::keyboard::KeyState::Press);
                    self.frame(&device);
                    self.keys.insert(code, device);
                } else if !down {
                    if let Some(device) = self.keys.remove(&code) {
                        if self.active.contains(&device) && device.device().is_alive() {
                            if let Some(keyboard) = device.interface::<ei::Keyboard>() {
                                keyboard.key(code, ei::keyboard::KeyState::Released);
                                self.frame(&device);
                            }
                        }
                    }
                }
            }
            InputCommand::Scroll { delta_x, delta_y } => {
                let (x, y) = keys::scroll_v120(delta_x, delta_y);
                if x != 0 || y != 0 {
                    let device = self.pointer_interface_device::<ei::Scroll>()?;
                    device
                        .interface::<ei::Scroll>()
                        .ok_or("Wayland scroll device disappeared.")?
                        .scroll_discrete(x, y);
                    self.frame(&device);
                }
            }
            InputCommand::ReleaseAll => self.release_all(),
            InputCommand::SecureAttention => {
                return Err("Secure attention is only supported on Windows.".into())
            }
        }
        self.flush()
    }

    pub fn disconnected(&mut self, connection: u64) -> Result<(), String> {
        // A delayed close from an older or unrelated connection must not
        // release keys belonging to the current controller's new connection.
        if self.controller == Some(connection) {
            self.release_all();
            self.controller = None;
            self.flush()?;
        }
        Ok(())
    }

    fn release_all(&mut self) {
        let mut changed = HashSet::new();
        for (code, device) in std::mem::take(&mut self.keys) {
            if self.active.contains(&device) && device.device().is_alive() {
                if let Some(keyboard) = device.interface::<ei::Keyboard>() {
                    keyboard.key(code, ei::keyboard::KeyState::Released);
                    changed.insert(device);
                }
            }
        }
        for (code, device) in std::mem::take(&mut self.buttons) {
            if self.active.contains(&device) && device.device().is_alive() {
                if let Some(button) = device.interface::<ei::Button>() {
                    button.button(code, ei::button::ButtonState::Released);
                    changed.insert(device);
                }
            }
        }
        for device in changed {
            self.frame(&device);
        }
    }

    pub fn flush(&self) -> Result<(), String> {
        // Including EAGAIN: fail closed instead of allowing libei's internal
        // socket buffer to grow indefinitely behind a stalled compositor.
        self.connection
            .flush()
            .map_err(|error| format!("Wayland EIS send failed: {error}. Restart input sharing."))
    }
}

impl Drop for EisInput {
    fn drop(&mut self) {
        self.release_all();
        for device in &self.active {
            if device.device().is_alive() {
                device.device().stop_emulating(self.connection.serial());
            }
        }
        let _ = self.connection.flush();
    }
}

fn is_keyboard_only(device: &Device) -> bool {
    device.has_capability(DeviceCapability::Keyboard)
        && device.regions().is_empty()
        && [
            DeviceCapability::Pointer,
            DeviceCapability::PointerAbsolute,
            DeviceCapability::Touch,
            DeviceCapability::Scroll,
            DeviceCapability::Button,
            DeviceCapability::Text,
        ]
        .into_iter()
        .all(|capability| !device.has_capability(capability))
}

fn monotonic_micros() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    (now.tv_sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add((now.tv_nsec as u64) / 1_000)
}

#[cfg(test)]
mod tests;
