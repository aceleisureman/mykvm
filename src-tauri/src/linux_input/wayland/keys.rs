//! The wire protocol uses Windows virtual keys. EIS keyboard.key expects Linux
//! evdev codes, *not* X11 keycodes (which have an extra offset of eight).

pub(super) fn evdev_key(vk: u16) -> Option<u32> {
    Some(match vk {
        0x41..=0x5A => [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ][usize::from(vk - 0x41)],
        0x30 => 11,
        0x31..=0x39 => u32::from(vk - 0x31) + 2,
        0x60..=0x69 => [82, 79, 80, 81, 75, 76, 77, 71, 72, 73][usize::from(vk - 0x60)],
        0x70..=0x79 => u32::from(vk - 0x70) + 59,
        0x7A => 87,
        0x7B => 88,
        0x7C..=0x87 => u32::from(vk - 0x7C) + 183,
        0x08 => 14,
        0x09 => 15,
        0x0C => 76,
        0x0D => 28,
        0x10 | 0xA0 => 42,
        0xA1 => 54,
        0x11 | 0xA2 => 29,
        0xA3 => 97,
        0x12 | 0xA4 => 56,
        0xA5 => 100,
        0x13 => 119,
        0x14 => 58,
        0x1B => 1,
        0x20 => 57,
        0x21 => 104,
        0x22 => 109,
        0x23 => 107,
        0x24 => 102,
        0x25 => 105,
        0x26 => 103,
        0x27 => 106,
        0x28 => 108,
        0x2C => 99,
        0x2D => 110,
        0x2E => 111,
        0x5B => 125,
        0x5C => 126,
        0x5D => 127,
        0x5F => 142,
        0x6A => 55,
        0x6B => 78,
        0x6C => 121,
        0x6D => 74,
        0x6E => 83,
        0x6F => 98,
        0x90 => 69,
        0x91 => 70,
        0xA6 => 158,
        0xA7 => 159,
        0xA8 => 173,
        0xA9 => 128,
        0xAA => 217,
        0xAB => 156,
        0xAC => 172,
        0xAD => 113,
        0xAE => 114,
        0xAF => 115,
        0xB0 => 163,
        0xB1 => 165,
        0xB2 => 166,
        0xB3 => 164,
        0xBA => 39,
        0xBB => 13,
        0xBC => 51,
        0xBD => 12,
        0xBE => 52,
        0xBF => 53,
        0xC0 => 41,
        0xDB => 26,
        0xDC => 43,
        0xDD => 27,
        0xDE => 40,
        0xE2 => 86,
        _ => return None,
    })
}

pub(super) fn scroll_v120(x: i32, y: i32) -> (i32, i32) {
    // Wire: positive Y is up. EIS: positive Y is down; one detent is 120.
    (x.clamp(-120, 120) * 120, -y.clamp(-120, 120) * 120)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_codes_are_evdev_not_x11_and_keep_sided_modifiers() {
        for (vk, evdev) in [
            (0x41, 30),
            (0x5A, 44),
            (0x30, 11),
            (0x39, 10),
            (0x10, 42),
            (0xA1, 54),
            (0x11, 29),
            (0xA3, 97),
            (0x12, 56),
            (0xA5, 100),
            (0x5B, 125),
            (0x5C, 126),
            (0x25, 105),
            (0x70, 59),
            (0x7B, 88),
            (0x87, 194),
            (0x60, 82),
            (0x6F, 98),
            (0xBA, 39),
            (0xE2, 86),
            (0xB3, 164),
        ] {
            assert_eq!(evdev_key(vk), Some(evdev));
        }
        assert_eq!(evdev_key(0xffff), None);
        assert_eq!(evdev_key(0), None);
    }
    #[test]
    fn wheel_direction_and_overflow_are_handled() {
        assert_eq!(scroll_v120(2, -3), (240, 360));
        assert_eq!(scroll_v120(-1, 1), (-120, -120));
        assert_eq!(scroll_v120(0, 0), (0, 0));
        assert_eq!(scroll_v120(i32::MIN, i32::MAX), (-14400, -14400));
    }
}
