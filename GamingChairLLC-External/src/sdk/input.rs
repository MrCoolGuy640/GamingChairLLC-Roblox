//! Win32 key helpers shared by the keybind poller, Fly movement and Fling's
//! stand-up interrupt.
//!
//! Labels are the ones the UI produces: `A`-`Z`, `0`-`9`, `F1`-`F24`, `Space`,
//! `Ctrl`, `Shift`, `Alt`, `Tab`, `Enter`, `Esc`, `Backspace`, `Delete`,
//! `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
//! and the punctuation keys as themselves.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

pub fn label_down(label: &str) -> bool {
    vk_from_label(label)
        .map(key_down)
        .unwrap_or(false)
}

/// Canonical key label -> Win32 virtual-key code.
pub fn vk_from_label(label: &str) -> Option<i32> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }

    // One character: letters, digits and punctuation.
    let mut chars = label.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let upper = ch.to_ascii_uppercase();
        if upper.is_ascii_uppercase() || upper.is_ascii_digit() {
            return Some(upper as i32);
        }
        if let Some(vk) = punctuation(ch) {
            return Some(vk);
        }
    }

    // F1..F24.
    if let Some(number) = label.strip_prefix('F').or_else(|| label.strip_prefix('f')) {
        if let Ok(number) = number.parse::<u32>() {
            if (1..=24).contains(&number) {
                return Some(0x70 + number as i32 - 1);
            }
        }
    }

    Some(match label.to_ascii_lowercase().as_str() {
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" => 0x12,
        "capslock" | "caps" => 0x14,
        _ => return None,
    })
}

fn punctuation(ch: char) -> Option<i32> {
    Some(match ch {
        '-' => 0xBD,
        '=' => 0xBB,
        '[' => 0xDB,
        ']' => 0xDD,
        '\\' => 0xDC,
        ';' => 0xBA,
        '\'' => 0xDE,
        ',' => 0xBC,
        '.' => 0xBE,
        '/' => 0xBF,
        '`' => 0xC0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::vk_from_label;

    #[test]
    fn letters_digits_and_function_keys_map_to_vk_codes() {
        assert_eq!(vk_from_label("A"), Some(0x41));
        assert_eq!(vk_from_label("z"), Some(0x5A));
        assert_eq!(vk_from_label("0"), Some(0x30));
        assert_eq!(vk_from_label("9"), Some(0x39));
        assert_eq!(vk_from_label("F1"), Some(0x70));
        assert_eq!(vk_from_label("F12"), Some(0x7B));
        assert_eq!(vk_from_label("f24"), Some(0x87));
    }

    #[test]
    fn named_keys_and_punctuation_map_to_vk_codes() {
        assert_eq!(vk_from_label("Space"), Some(0x20));
        assert_eq!(vk_from_label("Esc"), Some(0x1B));
        assert_eq!(vk_from_label("Backspace"), Some(0x08));
        assert_eq!(vk_from_label("Up"), Some(0x26));
        assert_eq!(vk_from_label("-"), Some(0xBD));
        assert_eq!(vk_from_label("`"), Some(0xC0));
    }

    #[test]
    fn empty_or_unknown_labels_are_unbound() {
        assert_eq!(vk_from_label(""), None);
        assert_eq!(vk_from_label("   "), None);
        assert_eq!(vk_from_label("F25"), None);
        assert_eq!(vk_from_label("Hyper"), None);
    }
}
