//! Global shortcut parsing ("Ctrl+Alt+R") into X11 terms.

/// X11 modifier masks.
pub const SHIFT: u16 = 1;
pub const LOCK: u16 = 2;
pub const CONTROL: u16 = 4;
pub const MOD1_ALT: u16 = 8;
pub const MOD2_NUMLOCK: u16 = 16;
pub const MOD4_SUPER: u16 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// X11 modifier mask that must be held.
    pub modifiers: u16,
    /// X11 keysym of the key.
    pub keysym: u32,
    /// Normalized text, e.g. "Ctrl+Alt+R".
    pub label: String,
}

impl Hotkey {
    /// Parses "Ctrl+Alt+R", "super+shift+F9", ...  Needs exactly one key, a
    /// letter, digit or F1–F12, and at least one modifier (a bare letter as a
    /// global shortcut would swallow typing).
    pub fn parse(s: &str) -> Result<Hotkey, String> {
        let mut modifiers = 0;
        let mut names = Vec::new();
        let mut key: Option<(u32, String)> = None;
        for part in s.split('+').map(str::trim) {
            let (m, name) = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => (CONTROL, "Ctrl"),
                "alt" => (MOD1_ALT, "Alt"),
                "shift" => (SHIFT, "Shift"),
                "super" | "win" | "meta" => (MOD4_SUPER, "Super"),
                _ => {
                    if key.is_some() {
                        return Err(format!("\"{s}\": more than one key"));
                    }
                    key = Some(
                        keysym(part).ok_or_else(|| format!("\"{s}\": unknown key \"{part}\""))?,
                    );
                    continue;
                }
            };
            if modifiers & m == 0 {
                names.push(name);
            }
            modifiers |= m;
        }
        let (keysym, key_name) = key.ok_or_else(|| format!("\"{s}\": no key"))?;
        if modifiers == 0 || modifiers == SHIFT {
            return Err(format!("\"{s}\": add Ctrl, Alt or Super"));
        }
        // Fixed order in the label.
        let order = ["Ctrl", "Alt", "Shift", "Super"];
        names.sort_by_key(|n| order.iter().position(|o| o == n));
        names.push(&key_name);
        Ok(Hotkey {
            modifiers,
            keysym,
            label: names.join("+"),
        })
    }

    /// Modifier masks to grab: the shortcut must also work with Caps Lock or
    /// Num Lock on.
    pub fn grab_masks(&self) -> [u16; 4] {
        let m = self.modifiers;
        [m, m | LOCK, m | MOD2_NUMLOCK, m | LOCK | MOD2_NUMLOCK]
    }

    /// Whether an X key event's state matches (ignoring the lock keys).
    pub fn matches_state(&self, state: u16) -> bool {
        let relevant = SHIFT | CONTROL | MOD1_ALT | MOD4_SUPER;
        state & relevant == self.modifiers
    }
}

fn keysym(name: &str) -> Option<(u32, String)> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphanumeric()
    {
        // Latin-1 keysyms equal the (lowercase) character code.
        return Some((
            c.to_ascii_lowercase() as u32,
            c.to_ascii_uppercase().to_string(),
        ));
    }
    let n: u32 = name.strip_prefix(['F', 'f'])?.parse().ok()?;
    (1..=12).contains(&n).then(|| (0xffbd + n, format!("F{n}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_shortcuts() {
        let h = Hotkey::parse("Ctrl+Alt+R").unwrap();
        assert_eq!(h.modifiers, CONTROL | MOD1_ALT);
        assert_eq!(h.keysym, 'r' as u32);
        assert_eq!(h.label, "Ctrl+Alt+R");
        let h = Hotkey::parse(" super + shift + f9 ").unwrap();
        assert_eq!(h.modifiers, MOD4_SUPER | SHIFT);
        assert_eq!(h.keysym, 0xffc6); // XK_F9
        assert_eq!(h.label, "Shift+Super+F9");
        assert_eq!(Hotkey::parse("alt+ctrl+5").unwrap().label, "Ctrl+Alt+5");
    }

    #[test]
    fn rejects_bad_shortcuts() {
        assert!(Hotkey::parse("R").is_err());
        assert!(Hotkey::parse("Shift+R").is_err());
        assert!(Hotkey::parse("Ctrl+Alt").is_err());
        assert!(Hotkey::parse("Ctrl+R+T").is_err());
        assert!(Hotkey::parse("Ctrl+Space").is_err());
        assert!(Hotkey::parse("Ctrl+F13").is_err());
    }

    #[test]
    fn lock_keys_do_not_matter() {
        let h = Hotkey::parse("Ctrl+Alt+R").unwrap();
        for m in h.grab_masks() {
            assert!(h.matches_state(m));
        }
        assert!(!h.matches_state(CONTROL));
        assert!(!h.matches_state(CONTROL | MOD1_ALT | SHIFT));
    }
}
