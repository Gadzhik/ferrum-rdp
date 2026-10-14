//! Linux evdev keycodes → RDP scancodes.
//!
//! RDP transmits PC/XT "set 1" scancodes plus an extended (E0) flag. evdev
//! codes 1..=88 coincide with set 1, everything else is looked up here.
//! GTK reports `hardware_keycode = evdev + 8` on both Wayland and X11.

/// Extended-key flag (KBDEXT) in the scancode passed to the shim.
pub const EXTENDED: u32 = 0x100;

const fn ext(code: u32) -> u32 {
    code | EXTENDED
}

/// RDP scancode for Pause/Break (sent as a special sequence by the shim).
pub const PAUSE: u32 = ext(0x46);
pub const LEFT_CTRL: u32 = 0x1D;
pub const LEFT_ALT: u32 = 0x38;
pub const LEFT_SHIFT: u32 = 0x2A;
pub const LEFT_WIN: u32 = ext(0x5B);
pub const DELETE: u32 = ext(0x53);
pub const ESCAPE: u32 = 0x01;
pub const TAB: u32 = 0x0F;
pub const ENTER: u32 = 0x1C;

/// Map an evdev keycode (not the X11 +8 keycode) to an RDP scancode.
pub fn evdev_to_rdp(code: u32) -> Option<u32> {
    let sc = match code {
        // KEY_ESC .. KEY_KPDOT: identical to set 1
        1..=83 => code,
        85 => 0x76,  // KEY_ZENKAKUHANKAKU
        86 => 0x56,  // KEY_102ND
        87 => 0x57,  // KEY_F11
        88 => 0x58,  // KEY_F12
        89 => 0x73,  // KEY_RO (ABNT_C1)
        90 => 0x78,  // KEY_KATAKANA
        91 => 0x77,  // KEY_HIRAGANA
        92 => 0x79,  // KEY_HENKAN (convert)
        93 => 0x70,  // KEY_KATAKANAHIRAGANA
        94 => 0x7B,  // KEY_MUHENKAN (nonconvert)
        95 => 0x5C,  // KEY_KPJPCOMMA
        96 => ext(0x1C),  // KEY_KPENTER
        97 => ext(0x1D),  // KEY_RIGHTCTRL
        98 => ext(0x35),  // KEY_KPSLASH
        99 => ext(0x37),  // KEY_SYSRQ / Print Screen
        100 => ext(0x38), // KEY_RIGHTALT
        102 => ext(0x47), // KEY_HOME
        103 => ext(0x48), // KEY_UP
        104 => ext(0x49), // KEY_PAGEUP
        105 => ext(0x4B), // KEY_LEFT
        106 => ext(0x4D), // KEY_RIGHT
        107 => ext(0x4F), // KEY_END
        108 => ext(0x50), // KEY_DOWN
        109 => ext(0x51), // KEY_PAGEDOWN
        110 => ext(0x52), // KEY_INSERT
        111 => ext(0x53), // KEY_DELETE
        113 => ext(0x20), // KEY_MUTE
        114 => ext(0x2E), // KEY_VOLUMEDOWN
        115 => ext(0x30), // KEY_VOLUMEUP
        116 => ext(0x5E), // KEY_POWER
        117 => 0x59,      // KEY_KPEQUAL
        119 => PAUSE,     // KEY_PAUSE
        121 => 0x7E,      // KEY_KPCOMMA (ABNT_C2)
        122 => 0x72,      // KEY_HANGEUL
        123 => 0x71,      // KEY_HANJA
        124 => 0x7D,      // KEY_YEN
        125 => ext(0x5B), // KEY_LEFTMETA
        126 => ext(0x5C), // KEY_RIGHTMETA
        127 => ext(0x5D), // KEY_COMPOSE (menu)
        128 => ext(0x68), // KEY_STOP (browser stop)
        138 => 0x63,      // KEY_HELP
        140 => ext(0x21), // KEY_CALC
        142 => ext(0x5F), // KEY_SLEEP
        143 => ext(0x63), // KEY_WAKEUP
        155 => ext(0x6C), // KEY_MAIL
        156 => ext(0x66), // KEY_BOOKMARKS (browser favorites)
        157 => ext(0x6B), // KEY_COMPUTER (my computer)
        158 => ext(0x6A), // KEY_BACK
        159 => ext(0x69), // KEY_FORWARD
        163 => ext(0x19), // KEY_NEXTSONG
        164 => ext(0x22), // KEY_PLAYPAUSE
        165 => ext(0x10), // KEY_PREVIOUSSONG
        166 => ext(0x24), // KEY_STOPCD
        171 => ext(0x6D), // KEY_CONFIG (media select)
        172 => ext(0x32), // KEY_HOMEPAGE
        173 => ext(0x67), // KEY_REFRESH
        183 => 0x64,      // KEY_F13
        184 => 0x65,
        185 => 0x66,
        186 => 0x67,
        187 => 0x68,
        188 => 0x69,
        189 => 0x6A,
        190 => 0x6B,
        191 => 0x6C,
        192 => 0x6D,
        193 => 0x6E,
        194 => 0x6F, // KEY_F24
        217 => ext(0x65), // KEY_SEARCH
        226 => ext(0x6D), // KEY_MEDIA
        _ => return None,
    };
    Some(sc)
}

/// Map a GTK/X11 hardware keycode (evdev + 8).
pub fn xkb_to_rdp(keycode: u32) -> Option<u32> {
    keycode.checked_sub(8).and_then(evdev_to_rdp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_keys_match_set1() {
        assert_eq!(evdev_to_rdp(1), Some(0x01)); // Esc
        assert_eq!(evdev_to_rdp(30), Some(0x1E)); // A
        assert_eq!(evdev_to_rdp(28), Some(0x1C)); // Enter
        assert_eq!(evdev_to_rdp(57), Some(0x39)); // Space
    }

    #[test]
    fn extended_keys() {
        assert_eq!(evdev_to_rdp(96), Some(0x11C)); // KP Enter
        assert_eq!(evdev_to_rdp(100), Some(0x138)); // AltGr
        assert_eq!(evdev_to_rdp(103), Some(0x148)); // Up
        assert_eq!(evdev_to_rdp(125), Some(0x15B)); // Super
        assert_eq!(evdev_to_rdp(119), Some(PAUSE));
    }

    #[test]
    fn xkb_offset() {
        assert_eq!(xkb_to_rdp(9), Some(0x01));
        assert_eq!(xkb_to_rdp(3), None);
        assert_eq!(xkb_to_rdp(8), None);
    }
}
