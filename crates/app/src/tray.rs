//! The icon in the top bar ("system tray") while recall runs: click to show
//! the window; the menu starts/stops recording or quits.  The icon turns red
//! while recording.
//!
//! GTK 4 has no tray API.  Ubuntu shows tray icons with its AppIndicator
//! extension, which implements the freedesktop StatusNotifierItem protocol:
//! the app exports two D-Bus objects and registers with the
//! `org.kde.StatusNotifierWatcher`.  This module does that with GIO directly.
//!
//! * `/StatusNotifierItem` (`org.kde.StatusNotifierItem`): icon, tooltip,
//!   and `Activate` (left click)
//! * `/MenuBar` (`com.canonical.dbusmenu`): the menu

use std::cell::Cell;
use std::rc::Rc;

use gtk::gio;
use gtk::glib::{self, Variant, VariantDict, VariantTy, prelude::*};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const XML: &str = r#"<node>
<interface name="org.kde.StatusNotifierItem">
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="AttentionIconName" type="s" access="read"/>
  <property name="IconThemePath" type="s" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  <property name="Menu" type="o" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
  <signal name="NewTitle"/>
  <signal name="NewIcon"/>
  <signal name="NewAttentionIcon"/>
  <signal name="NewToolTip"/>
  <signal name="NewStatus"><arg name="status" type="s"/></signal>
</interface>
<interface name="com.canonical.dbusmenu">
  <property name="Version" type="u" access="read"/>
  <property name="TextDirection" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconThemePath" type="as" access="read"/>
  <method name="GetLayout">
    <arg name="parentId" type="i" direction="in"/>
    <arg name="recursionDepth" type="i" direction="in"/>
    <arg name="propertyNames" type="as" direction="in"/>
    <arg name="revision" type="u" direction="out"/>
    <arg name="layout" type="(ia{sv}av)" direction="out"/>
  </method>
  <method name="GetGroupProperties">
    <arg name="ids" type="ai" direction="in"/>
    <arg name="propertyNames" type="as" direction="in"/>
    <arg name="properties" type="a(ia{sv})" direction="out"/>
  </method>
  <method name="GetProperty">
    <arg name="id" type="i" direction="in"/>
    <arg name="name" type="s" direction="in"/>
    <arg name="value" type="v" direction="out"/>
  </method>
  <method name="Event">
    <arg name="id" type="i" direction="in"/>
    <arg name="eventId" type="s" direction="in"/>
    <arg name="data" type="v" direction="in"/>
    <arg name="timestamp" type="u" direction="in"/>
  </method>
  <method name="EventGroup">
    <arg name="events" type="a(isvu)" direction="in"/>
    <arg name="idErrors" type="ai" direction="out"/>
  </method>
  <method name="AboutToShow">
    <arg name="id" type="i" direction="in"/>
    <arg name="needUpdate" type="b" direction="out"/>
  </method>
  <method name="AboutToShowGroup">
    <arg name="ids" type="ai" direction="in"/>
    <arg name="updatesNeeded" type="ai" direction="out"/>
    <arg name="idErrors" type="ai" direction="out"/>
  </method>
  <signal name="ItemsPropertiesUpdated">
    <arg name="updatedProps" type="a(ia{sv})"/>
    <arg name="removedProps" type="a(ias)"/>
  </signal>
  <signal name="LayoutUpdated">
    <arg name="revision" type="u"/>
    <arg name="parent" type="i"/>
  </signal>
</interface>
</node>"#;

/// Menu item ids.
const ROOT: i32 = 0;
const OPEN: i32 = 1;
const RECORD: i32 = 2;
const SEPARATOR: i32 = 3;
const QUIT: i32 = 4;
const ITEMS: [i32; 5] = [ROOT, OPEN, RECORD, SEPARATOR, QUIT];

/// Icon theme names.
pub struct Icons {
    pub idle: String,
    pub recording: String,
}

/// What the icon shows; shared by the D-Bus handlers.
struct Shown {
    recording: Cell<bool>,
    revision: Cell<u32>,
    icons: Icons,
}

impl Shown {
    fn icon(&self) -> &str {
        if self.recording.get() {
            &self.icons.recording
        } else {
            &self.icons.idle
        }
    }

    fn tooltip(&self) -> Variant {
        let text = if self.recording.get() {
            "Recording"
        } else {
            "Not recording"
        };
        let pixmaps = Variant::array_from_iter_with_type(
            VariantTy::new("(iiay)").unwrap(),
            std::iter::empty::<Variant>(),
        );
        Variant::tuple_from_iter([
            self.icon().to_variant(),
            pixmaps,
            "recall".to_variant(),
            text.to_variant(),
        ])
    }

    fn props(&self, id: i32) -> Vec<(&'static str, Variant)> {
        let label = |s: &str| vec![("label", s.to_variant())];
        match id {
            ROOT => vec![("children-display", "submenu".to_variant())],
            OPEN => label("Open recall"),
            RECORD if self.recording.get() => label("■ Stop recording"),
            RECORD => label("● Record"),
            SEPARATOR => vec![("type", "separator".to_variant())],
            QUIT => label("Quit"),
            _ => vec![],
        }
    }

    /// `(ia{sv}av)` for `id` and its children.
    fn layout(&self, id: i32) -> Variant {
        let children: Vec<Variant> = if id == ROOT {
            ITEMS[1..]
                .iter()
                .map(|&c| Variant::from_variant(&self.layout(c)))
                .collect()
        } else {
            vec![]
        };
        Variant::tuple_from_iter([
            id.to_variant(),
            dict(&self.props(id)),
            Variant::array_from_iter_with_type(VariantTy::VARIANT, children),
        ])
    }
}

fn dict(props: &[(&str, Variant)]) -> Variant {
    let d = VariantDict::new(None);
    for (k, v) in props {
        d.insert_value(k, v);
    }
    d.end()
}

/// What the icon and its menu do.
pub struct Actions {
    pub open: Box<dyn Fn()>,
    pub toggle: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
}

pub struct Tray {
    conn: gio::DBusConnection,
    shown: Rc<Shown>,
    registrations: Vec<gio::RegistrationId>,
    /// Stops watching for the StatusNotifierWatcher.
    unwatch: Option<Box<dyn FnOnce()>>,
}

impl Tray {
    /// Exports the icon on `conn` (a session bus connection).
    pub fn start(
        conn: &gio::DBusConnection,
        icons: Icons,
        actions: Actions,
    ) -> Result<Tray, glib::Error> {
        let node = gio::DBusNodeInfo::for_xml(XML)?;
        let item_iface = node.lookup_interface(ITEM_IFACE).unwrap();
        let menu_iface = node.lookup_interface(MENU_IFACE).unwrap();
        let shown = Rc::new(Shown {
            recording: Cell::new(false),
            revision: Cell::new(1),
            icons,
        });
        let actions = Rc::new(actions);

        let s = shown.clone();
        let a = actions.clone();
        let item = conn
            .register_object(ITEM_PATH, &item_iface)
            .method_call(move |_, _, _, _, method, _, inv| {
                if matches!(method, "Activate" | "SecondaryActivate") {
                    clicked(OPEN, &a);
                }
                inv.return_value(None);
            })
            .property(move |_, _, _, _, prop| match prop {
                "Category" => "ApplicationStatus".to_variant(),
                "Id" => "recall".to_variant(),
                "Title" => "recall".to_variant(),
                "Status" => "Active".to_variant(),
                "IconName" => s.icon().to_variant(),
                "AttentionIconName" => s.icons.recording.to_variant(),
                "IconThemePath" => "".to_variant(),
                "ItemIsMenu" => false.to_variant(),
                "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH)
                    .unwrap()
                    .to_variant(),
                "ToolTip" => s.tooltip(),
                _ => "".to_variant(),
            })
            .build()?;

        let s = shown.clone();
        let menu = conn
            .register_object(MENU_PATH, &menu_iface)
            .method_call(move |_, _, _, _, method, params, inv| {
                let reply = match method {
                    "GetLayout" => {
                        let parent = params.child_value(0).get::<i32>().unwrap_or(ROOT);
                        Some(Variant::tuple_from_iter([
                            s.revision.get().to_variant(),
                            s.layout(parent),
                        ]))
                    }
                    "GetGroupProperties" => {
                        let ids = params.child_value(0).get::<Vec<i32>>().unwrap_or_default();
                        let ids = if ids.is_empty() { ITEMS.to_vec() } else { ids };
                        let items = ids.into_iter().map(|id| {
                            Variant::tuple_from_iter([id.to_variant(), dict(&s.props(id))])
                        });
                        Some(Variant::tuple_from_iter([
                            Variant::array_from_iter_with_type(
                                VariantTy::new("(ia{sv})").unwrap(),
                                items,
                            ),
                        ]))
                    }
                    "GetProperty" => {
                        let id = params.child_value(0).get::<i32>().unwrap_or(-1);
                        let name = params.child_value(1).get::<String>().unwrap_or_default();
                        let value = s
                            .props(id)
                            .into_iter()
                            .find(|(k, _)| *k == name)
                            .map_or_else(|| "".to_variant(), |(_, v)| v);
                        Some(Variant::tuple_from_iter([Variant::from_variant(&value)]))
                    }
                    "Event" => {
                        let id = params.child_value(0).get::<i32>().unwrap_or(-1);
                        let event = params.child_value(1).get::<String>().unwrap_or_default();
                        if event == "clicked" {
                            clicked(id, &actions);
                        }
                        None
                    }
                    "EventGroup" => {
                        let events = params.child_value(0);
                        for e in events.iter() {
                            let id = e.child_value(0).get::<i32>().unwrap_or(-1);
                            if e.child_value(1).get::<String>().as_deref() == Some("clicked") {
                                clicked(id, &actions);
                            }
                        }
                        Some((Vec::<i32>::new(),).to_variant())
                    }
                    "AboutToShow" => Some((false,).to_variant()),
                    "AboutToShowGroup" => Some((Vec::<i32>::new(), Vec::<i32>::new()).to_variant()),
                    _ => None,
                };
                inv.return_value(reply.as_ref());
            })
            .property(move |_, _, _, _, prop| match prop {
                "Version" => 3u32.to_variant(),
                "TextDirection" => "ltr".to_variant(),
                "Status" => "normal".to_variant(),
                "IconThemePath" => Vec::<String>::new().to_variant(),
                _ => "".to_variant(),
            })
            .build()?;

        // Register now and whenever the watcher (re)appears (GNOME Shell
        // restarts, the extension gets enabled later).
        let unique = conn.unique_name().map(|n| n.to_string());
        let unwatch = unique.map(|unique| {
            let id = gio::bus_watch_name_on_connection(
                conn,
                WATCHER,
                gio::BusNameWatcherFlags::NONE,
                move |conn, _, _| {
                    conn.call(
                        Some(WATCHER),
                        "/StatusNotifierWatcher",
                        WATCHER,
                        "RegisterStatusNotifierItem",
                        Some(&(unique.as_str(),).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        -1,
                        gio::Cancellable::NONE,
                        |r| {
                            if let Err(e) = r {
                                crate::log::write(&format!("tray icon: {e}"));
                            }
                        },
                    );
                },
                |_, _| {},
            );
            Box::new(move || gio::bus_unwatch_name(id)) as Box<dyn FnOnce()>
        });

        Ok(Tray {
            conn: conn.clone(),
            shown,
            registrations: vec![item, menu],
            unwatch,
        })
    }

    /// Red icon and "Stop recording" in the menu while recording.
    pub fn set_recording(&self, recording: bool) {
        if self.shown.recording.replace(recording) == recording {
            return;
        }
        let s = &self.shown;
        s.revision.set(s.revision.get() + 1);
        let emit = |path: &str, iface: &str, signal: &str, args: Option<Variant>| {
            if let Err(e) = self
                .conn
                .emit_signal(None, path, iface, signal, args.as_ref())
            {
                crate::log::write(&format!("tray icon: {e}"));
            }
        };
        emit(ITEM_PATH, ITEM_IFACE, "NewIcon", None);
        emit(ITEM_PATH, ITEM_IFACE, "NewToolTip", None);
        emit(
            MENU_PATH,
            MENU_IFACE,
            "LayoutUpdated",
            Some((s.revision.get(), ROOT).to_variant()),
        );
    }
}

fn clicked(id: i32, actions: &Rc<Actions>) {
    if !matches!(id, OPEN | RECORD | QUIT) {
        return;
    }
    // Not from inside the D-Bus handler: quitting ends the main loop.
    let a = actions.clone();
    glib::idle_add_local_once(move || match id {
        OPEN => (a.open)(),
        RECORD => (a.toggle)(),
        _ => (a.quit)(),
    });
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.take() {
            unwatch();
        }
        for r in self.registrations.drain(..) {
            let _ = self.conn.unregister_object(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(recording: bool) -> Shown {
        Shown {
            recording: Cell::new(recording),
            revision: Cell::new(1),
            icons: Icons {
                idle: "i".into(),
                recording: "r".into(),
            },
        }
    }

    fn label(layout: &Variant, n: usize) -> Option<String> {
        let item = layout.child_value(2).child_value(n).as_variant().unwrap();
        VariantDict::new(Some(&item.child_value(1)))
            .lookup::<String>("label")
            .unwrap()
    }

    #[test]
    fn idle() {
        let s = shown(false);
        let layout = s.layout(ROOT);
        assert_eq!(layout.type_().as_str(), "(ia{sv}av)");
        assert_eq!(layout.child_value(2).n_children(), 4);
        assert_eq!(label(&layout, 0).as_deref(), Some("Open recall"));
        assert_eq!(label(&layout, 1).as_deref(), Some("● Record"));
        assert_eq!(label(&layout, 3).as_deref(), Some("Quit"));
        assert_eq!(s.icon(), "i");
        assert_eq!(s.tooltip().type_().as_str(), "(sa(iiay)ss)");
    }

    #[test]
    fn recording() {
        let s = shown(true);
        assert_eq!(s.icon(), "r");
        assert_eq!(
            label(&s.layout(ROOT), 1).as_deref(),
            Some("■ Stop recording")
        );
        let text = s.tooltip().child_value(3).get::<String>().unwrap();
        assert_eq!(text, "Recording");
    }
}
