//! Global shortcut on X11: grabs the key on the root window, so it works
//! while Chrome/Zoom has the focus.  (Wayland has no such API for apps.)

use recall_core::hotkey::Hotkey;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};

/// Starts listening for `hotkey`; `on_press` runs (on a background thread)
/// for every press.  Errors if there is no X11 display or another program
/// already uses the shortcut.
pub fn listen(hotkey: Hotkey, on_press: impl Fn() + Send + 'static) -> anyhow::Result<()> {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland") {
        anyhow::bail!("global shortcuts need an X11 session (this is Wayland)");
    }
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let keycode = keycode_for(&conn, hotkey.keysym)?
        .ok_or_else(|| anyhow::anyhow!("no key on this keyboard for {}", hotkey.label))?;
    for mask in hotkey.grab_masks() {
        conn.grab_key(
            true,
            root,
            ModMask::from(mask),
            keycode,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )?
        .check()
        .map_err(|_| anyhow::anyhow!("{} is already used by another program", hotkey.label))?;
    }
    crate::log::write(&format!(
        "global shortcut {} (keycode {keycode})",
        hotkey.label
    ));
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            while let Ok(ev) = conn.wait_for_event() {
                if let Event::KeyPress(k) = ev
                    && k.detail == keycode
                    && hotkey.matches_state(u16::from(k.state))
                {
                    on_press();
                }
            }
        })?;
    Ok(())
}

fn keycode_for(conn: &impl Connection, keysym: u32) -> anyhow::Result<Option<u8>> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    Ok(map
        .keysyms
        .chunks(per.max(1))
        .position(|syms| syms.contains(&keysym))
        .map(|i| min + i as u8))
}
