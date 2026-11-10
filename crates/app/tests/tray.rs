//! The tray icon's D-Bus objects, on a private session bus (`gio::TestDBus`
//! starts its own dbus-daemon), as GNOME Shell's AppIndicator extension sees
//! them.

use std::cell::Cell;
use std::rc::Rc;

use gtk::gio;
use gtk::glib::{self, prelude::*};
use recall::tray::{Actions, Icons, Tray};

/// A D-Bus call made from another connection; returns its result as text.
type ClientCall = Box<dyn FnOnce(&gio::DBusConnection, &str) -> String + Send>;

/// Runs the main loop until `done` or a timeout.
fn spin(ctx: &glib::MainContext, done: impl Fn() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() && std::time::Instant::now() < until {
        ctx.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn tray_icon_menu_and_recording_state() {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let ctx = glib::MainContext::default();
    let _guard = ctx.acquire().unwrap();
    let conn = gio::DBusConnection::for_address_sync(
        &bus.bus_address().unwrap(),
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .unwrap();

    let opened = Rc::new(Cell::new(0));
    let toggled = Rc::new(Cell::new(0));
    let quit = Rc::new(Cell::new(0));
    let (o, t, q) = (opened.clone(), toggled.clone(), quit.clone());
    let tray = Tray::start(
        &conn,
        Icons {
            idle: "idle-icon".into(),
            recording: "rec-icon".into(),
        },
        Actions {
            open: Box::new(move || o.set(o.get() + 1)),
            toggle: Box::new(move || t.set(t.get() + 1)),
            quit: Box::new(move || q.set(q.get() + 1)),
        },
    )
    .unwrap();

    // Calls to our own connection need the main loop to answer: run them on
    // a thread with a second connection instead.
    let addr = bus.bus_address().unwrap().to_string();
    let me = conn.unique_name().unwrap().to_string();
    let client = move |f: ClientCall| {
        let addr = addr.clone();
        let me = me.clone();
        let h = std::thread::spawn(move || {
            let c = gio::DBusConnection::for_address_sync(
                &addr,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                    | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
                gio::Cancellable::NONE,
            )
            .unwrap();
            f(&c, &me)
        });
        let ctx = glib::MainContext::default();
        spin(&ctx, || h.is_finished());
        h.join().unwrap()
    };

    let icon = |c: &gio::DBusConnection, me: &str| -> String {
        c.call_sync(
            Some(me),
            "/StatusNotifierItem",
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("org.kde.StatusNotifierItem", "IconName").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .unwrap()
        .child_value(0)
        .as_variant()
        .unwrap()
        .get::<String>()
        .unwrap()
    };
    let layout = |c: &gio::DBusConnection, me: &str| -> String {
        c.call_sync(
            Some(me),
            "/MenuBar",
            "com.canonical.dbusmenu",
            "GetLayout",
            Some(&(0i32, -1i32, Vec::<String>::new()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .unwrap()
        .print(false)
        .to_string()
    };
    let click = |id: i32| {
        move |c: &gio::DBusConnection, me: &str| -> String {
            c.call_sync(
                Some(me),
                "/MenuBar",
                "com.canonical.dbusmenu",
                "Event",
                Some(&(id, "clicked", "".to_variant(), 0u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
            )
            .unwrap();
            String::new()
        }
    };

    assert_eq!(client(Box::new(icon)), "idle-icon");
    let menu = client(Box::new(layout));
    assert!(menu.contains("Open recall"), "{menu}");
    assert!(menu.contains("● Record"), "{menu}");
    assert!(menu.contains("Quit"), "{menu}");

    // Menu items run their action (from the main loop).
    client(Box::new(click(2)));
    spin(&ctx, || toggled.get() == 1);
    assert_eq!(toggled.get(), 1);
    client(Box::new(click(1)));
    spin(&ctx, || opened.get() == 1);
    assert_eq!(opened.get(), 1);
    client(Box::new(click(4)));
    spin(&ctx, || quit.get() == 1);
    assert_eq!(quit.get(), 1);
    // Left click on the icon opens the window.
    client(Box::new(|c: &gio::DBusConnection, me: &str| {
        c.call_sync(
            Some(me),
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem",
            "Activate",
            Some(&(0i32, 0i32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .unwrap();
        String::new()
    }));
    spin(&ctx, || opened.get() == 2);
    assert_eq!(opened.get(), 2);

    // Recording: red icon, "Stop recording" in the menu.
    tray.set_recording(true);
    assert_eq!(client(Box::new(icon)), "rec-icon");
    let menu = client(Box::new(layout));
    assert!(menu.contains("Stop recording"), "{menu}");
    tray.set_recording(false);
    assert_eq!(client(Box::new(icon)), "idle-icon");

    drop(tray);
    bus.down();
}
