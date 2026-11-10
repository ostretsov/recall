//! The main window: record button, language, level meters, live subtitles.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use recall_core::call::{self, MIN_FREE_BYTES, human_size};
use recall_core::config::{self, Config, Paths};
use recall_core::control::{Command, Reply, State};
use recall_core::deps;
use recall_core::hotkey::Hotkey;
use recall_core::lang::{Language, ModelInfo};
use recall_core::levels;
use recall_core::mic_channel;
use recall_core::pipelines::Source;
use recall_core::transcript::{Segment, Speaker, clock};
use recall_core::whisper;

use crate::control::Request;
use crate::jobs::{self, JobEvent};
use crate::models::{self, VoskCache};
use crate::recorder::{Event, Finished, Options, Recorder, SubtitleStatus};

const APP_ID: &str = "io.github.ostretsov.recall";

const CSS: &str = "
.subtitles text { background: transparent; }
.record { font-weight: bold; padding: 6px 18px; }
.elapsed { font-size: 15pt; font-feature-settings: 'tnum'; }
.banner { background: alpha(@accent_bg_color, 0.15); border-radius: 6px; padding: 8px; }
.status-error { color: #c01c28; }
.update-bar { padding: 4px 8px; border-radius: 6px; background: alpha(@accent_bg_color, 0.15); }
";

pub struct Args {
    pub config_dir: Option<PathBuf>,
    pub control: Option<PathBuf>,
}

pub fn run(args: Args) -> glib::ExitCode {
    crate::log::init();
    if let Err(e) = gst::init() {
        eprintln!("GStreamer is not available: {e}");
        return glib::ExitCode::FAILURE;
    }
    vosk::set_log_level(vosk::LogLevel::Error);
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let args = Rc::new(args);
    app.connect_activate(move |gapp| {
        load_css();
        let missing = deps::missing_packages(
            |e| gst::ElementFactory::find(e).is_some(),
            crate::selftest::in_path,
        );
        if !missing.is_empty() {
            show_fatal(
                gapp,
                &format!(
                    "Some system packages are missing. Install them with:\n\nsudo apt install {}",
                    missing.join(" ")
                ),
            );
            return;
        }
        if let Err(e) = App::build(gapp, &args) {
            show_fatal(gapp, &format!("{e:#}"));
        }
    });
    app.run_with_args(&["recall"])
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    #[allow(deprecated)]
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        #[allow(deprecated)]
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn show_fatal(gapp: &gtk::Application, text: &str) {
    eprintln!("[recall] {text}");
    crate::log::write(text);
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .selectable(true)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    gtk::ApplicationWindow::builder()
        .application(gapp)
        .title("recall - error")
        .child(&label)
        .build()
        .present();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Recording,
    Stopping,
}

struct St {
    config: Config,
    phase: Phase,
    recorder: Option<Recorder>,
    started: Option<Instant>,
    call_dir: Option<PathBuf>,
    segments: Vec<Segment>,
    partial: [String; 2],
    levels: [f64; 2],
    status: String,
    status_is_error: bool,
    downloading: bool,
    hotkey: Option<String>,
    /// Quit once the recording is saved.
    quit_after_stop: bool,
    /// The graphics card for transcripts: `None` until detected.
    gpu: Option<Option<String>>,
    /// Calls waiting for their transcript.
    queue: VecDeque<PathBuf>,
    /// The transcription running now, and its progress 0..1.
    running: Option<jobs::Running>,
    job_progress: f64,
    /// Calls to transcribe on the CPU (the GPU attempt crashed).
    cpu_only: HashSet<PathBuf>,
    /// The last finished transcript.
    last_transcript: Option<PathBuf>,
}

struct App {
    gapp: gtk::Application,
    window: gtk::ApplicationWindow,
    record: gtk::Button,
    elapsed: gtk::Label,
    language: gtk::DropDown,
    settings: Settings,
    meters: [gtk::LevelBar; 2],
    buffer: gtk::TextBuffer,
    view: gtk::TextView,
    /// Where the partial (still changing) lines start.
    partial_mark: gtk::TextMark,
    end_mark: gtk::TextMark,
    banner: gtk::Box,
    banner_label: gtk::Label,
    banner_button: gtk::Button,
    banner_progress: gtk::ProgressBar,
    status: gtk::Label,
    footer: gtk::Label,
    job_row: gtk::Box,
    job_label: gtk::Label,
    job_bar: gtk::ProgressBar,
    job_open: gtk::Button,
    updater: crate::update::Updater,
    tray: RefCell<Option<crate::tray::Tray>>,
    jobs_tx: async_channel::Sender<JobEvent>,
    config_dir: Option<PathBuf>,
    /// Holds the `.subtitles` font size rule.
    font_css: gtk::CssProvider,
    paths: Paths,
    vosk: VoskCache,
    events: async_channel::Sender<Event>,
    st: RefCell<St>,
}

fn idx(s: Speaker) -> usize {
    match s {
        Speaker::Me => 0,
        Speaker::Others => 1,
    }
}

impl App {
    fn build(gapp: &gtk::Application, args: &Args) -> anyhow::Result<Rc<App>> {
        let paths = Paths::from_env(args.config_dir.clone());
        let config = match Config::load(&paths.config_file()) {
            Ok(c) => c,
            Err(e) => {
                crate::log::write(&format!("{e:#}; using defaults"));
                Config::default()
            }
        };

        let window = gtk::ApplicationWindow::builder()
            .application(gapp)
            .title("recall")
            .icon_name(APP_ID)
            .default_width(560)
            .default_height(640)
            .build();
        let header = gtk::HeaderBar::new();
        let settings = Settings::new(&config);
        header.pack_end(&settings.button);
        window.set_titlebar(Some(&header));

        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();

        let updater = crate::update::Updater::new();
        vbox.append(&updater.root);

        // Record button, elapsed time, language.
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let record = gtk::Button::with_label("● Record");
        record.add_css_class("record");
        record.add_css_class("suggested-action");
        let elapsed = gtk::Label::new(Some("0:00:00"));
        elapsed.add_css_class("elapsed");
        elapsed.add_css_class("dim-label");
        let language = settings.language.clone();
        top.append(&record);
        top.append(&elapsed);
        vbox.append(&top);

        // Level meters.
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(4)
            .build();
        let meters = Speaker::ALL.map(|sp| {
            let label = gtk::Label::new(Some(sp.label()));
            label.set_xalign(0.0);
            let bar = gtk::LevelBar::new();
            bar.set_hexpand(true);
            bar.set_valign(gtk::Align::Center);
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_LOW));
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_HIGH));
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_FULL));
            let row = idx(sp) as i32;
            grid.attach(&label, 0, row, 1, 1);
            grid.attach(&bar, 1, row, 1, 1);
            bar
        });
        vbox.append(&grid);

        // Model download banner.
        let banner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        banner.add_css_class("banner");
        let brow = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let banner_label = gtk::Label::new(None);
        banner_label.set_wrap(true);
        banner_label.set_xalign(0.0);
        banner_label.set_hexpand(true);
        let banner_button = gtk::Button::with_label("Download");
        brow.append(&banner_label);
        brow.append(&banner_button);
        let banner_progress = gtk::ProgressBar::new();
        banner_progress.set_visible(false);
        banner.append(&brow);
        banner.append(&banner_progress);
        banner.set_visible(false);
        vbox.append(&banner);

        // Subtitles.
        let buffer = gtk::TextBuffer::new(None);
        for (name, props) in [
            ("time", vec![("foreground", "#888888")]),
            (
                "me",
                vec![("foreground", "#1c71d8"), ("weight-set", "true")],
            ),
            (
                "others",
                vec![("foreground", "#26a269"), ("weight-set", "true")],
            ),
            (
                "partial",
                vec![("foreground", "#888888"), ("style-set", "true")],
            ),
            ("hint", vec![("foreground", "#888888")]),
        ] {
            let tag = gtk::TextTag::new(Some(name));
            for (k, v) in props {
                match k {
                    "foreground" => tag.set_foreground(Some(v)),
                    "weight-set" => tag.set_weight(700),
                    "style-set" => tag.set_style(gtk::pango::Style::Italic),
                    _ => {}
                }
            }
            buffer.tag_table().add(&tag);
        }
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        view.add_css_class("subtitles");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&scroll));
        vbox.append(&frame);
        let partial_mark = buffer.create_mark(Some("partial"), &buffer.end_iter(), false);
        let end_mark = buffer.create_mark(Some("end"), &buffer.end_iter(), false);

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_selectable(true);
        vbox.append(&status);
        // Transcript progress / result.
        let job_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let job_label = gtk::Label::new(None);
        job_label.set_xalign(0.0);
        job_label.set_hexpand(true);
        job_label.set_wrap(true);
        let job_bar = gtk::ProgressBar::new();
        job_bar.set_valign(gtk::Align::Center);
        job_bar.set_size_request(120, -1);
        let job_open = gtk::Button::with_label("Open folder");
        job_open.set_tooltip_text(Some("The call's folder: transcript and recordings"));
        job_row.append(&job_label);
        job_row.append(&job_bar);
        job_row.append(&job_open);
        job_row.set_visible(false);
        vbox.append(&job_row);
        let footer = gtk::Label::new(None);
        footer.set_xalign(0.0);
        footer.set_wrap(true);
        footer.add_css_class("dim-label");
        footer.add_css_class("caption");
        vbox.append(&footer);
        window.set_child(Some(&vbox));

        let (events, events_rx) = async_channel::unbounded();
        let (jobs_tx, jobs_rx) = async_channel::unbounded::<JobEvent>();
        let app = Rc::new(App {
            gapp: gapp.clone(),
            window: window.clone(),
            record: record.clone(),
            elapsed,
            language: language.clone(),
            settings,
            meters,
            buffer,
            view,
            partial_mark,
            end_mark,
            banner,
            banner_label,
            banner_button: banner_button.clone(),
            banner_progress,
            status,
            footer,
            job_row,
            job_label,
            job_bar,
            job_open: job_open.clone(),
            updater,
            tray: RefCell::new(None),
            jobs_tx,
            config_dir: args.config_dir.clone(),
            font_css: gtk::CssProvider::new(),
            paths,
            vosk: VoskCache::default(),
            events,
            st: RefCell::new(St {
                config,
                phase: Phase::Idle,
                recorder: None,
                started: None,
                call_dir: None,
                segments: Vec::new(),
                partial: Default::default(),
                levels: [f64::NEG_INFINITY; 2],
                status: String::new(),
                status_is_error: false,
                downloading: false,
                hotkey: None,
                quit_after_stop: false,
                gpu: None,
                queue: VecDeque::new(),
                running: None,
                job_progress: 0.0,
                cpu_only: HashSet::new(),
                last_transcript: None,
            }),
        });
        let a = app.clone();
        job_open.connect_clicked(move |_| {
            let dir =
                a.st.borrow()
                    .last_transcript
                    .as_ref()
                    .and_then(|p| p.parent().map(Path::to_path_buf));
            if let Some(dir) = dir {
                a.open_path(&dir);
            }
        });
        let a = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = jobs_rx.recv().await {
                a.on_job_event(ev);
            }
        });

        // Wiring.
        let a = app.clone();
        record.connect_clicked(move |_| a.toggle());
        let a = app.clone();
        language.connect_selected_notify(move |dd| {
            let lang = Language::ALL[(dd.selected() as usize).min(Language::ALL.len() - 1)];
            a.set_language(lang);
        });
        let a = app.clone();
        banner_button.connect_clicked(move |_| a.download_model());
        let a = app.clone();
        app.settings.open.connect_clicked(move |_| a.open_folder());
        let a = app.clone();
        app.settings.smaller.connect_clicked(move |_| a.zoom(-1.0));
        let a = app.clone();
        app.settings.bigger.connect_clicked(move |_| a.zoom(1.0));
        let a = app.clone();
        app.settings
            .folder
            .connect_clicked(move |_| a.choose_output_dir());
        let a = app.clone();
        app.settings
            .autostart
            .connect_active_notify(move |sw| a.set_autostart(sw.is_active()));
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);
        for (keys, step) in [
            ("<Control>plus|<Control>equal|<Control>KP_Add", 1.0),
            ("<Control>minus|<Control>KP_Subtract", -1.0),
            ("<Control>0|<Control>KP_0", 0.0),
        ] {
            let a = Rc::downgrade(&app);
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(a) = a.upgrade() {
                    a.zoom(step);
                }
                glib::Propagation::Stop
            });
            if let Some(trigger) = gtk::ShortcutTrigger::parse_string(keys) {
                shortcuts.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
            }
        }
        window.add_controller(shortcuts);
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let a = Rc::downgrade(&app);
        wheel.connect_scroll(move |c, _, dy| {
            let ctrl = c
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            match a.upgrade() {
                Some(a) if ctrl && dy != 0.0 => {
                    a.zoom(if dy < 0.0 { 1.0 } else { -1.0 });
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        app.view.add_controller(wheel);
        if let Some(display) = gdk::Display::default() {
            #[allow(deprecated)]
            gtk::style_context_add_provider_for_display(
                &display,
                &app.font_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        let size = app.st.borrow().config.font_size;
        app.apply_font_size(size);
        let a = app.clone();
        window.connect_close_request(move |w| {
            // With the top bar icon the app keeps running (recording,
            // transcripts, the shortcut); its menu brings the window back
            // or quits.  Without the icon there would be no way back.
            if a.tray.borrow().as_ref().is_some_and(|t| t.is_visible()) {
                crate::log::write("window closed; still running in the top bar");
                w.set_visible(false);
                return glib::Propagation::Stop;
            }
            a.quit();
            glib::Propagation::Stop
        });
        // "app.quit": the update banner's Restart button.
        let quit = gio::SimpleAction::new("quit", None);
        let a = Rc::downgrade(&app);
        quit.connect_activate(move |_, _| {
            if let Some(a) = a.upgrade() {
                a.quit();
            }
        });
        gapp.add_action(&quit);
        let a = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = events_rx.recv().await {
                a.on_event(ev);
            }
        });
        let a = Rc::downgrade(&app);
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let Some(a) = a.upgrade() else {
                return glib::ControlFlow::Break;
            };
            a.tick();
            glib::ControlFlow::Continue
        });

        app.setup_hotkey();
        app.sync_autostart();
        app.start_tray();
        let a = Rc::downgrade(&app);
        app.updater.set_recording_check(move || {
            a.upgrade()
                .is_some_and(|a| a.st.borrow().phase != Phase::Idle)
        });
        if app.st.borrow().config.check_updates {
            app.updater.check(false);
        }
        if let Some(path) = &args.control {
            let (tx, rx) = async_channel::unbounded::<Request>();
            crate::control::serve(path, tx)?;
            let a = app.clone();
            glib::spawn_future_local(async move {
                while let Ok((cmd, reply)) = rx.recv().await {
                    let _ = reply.send(a.handle_control(cmd));
                }
            });
        }

        // Which graphics card (if any) transcripts will use decides the
        // model; find out in the background, then start pending work.
        let (gtx, grx) = async_channel::bounded::<Option<String>>(1);
        std::thread::spawn(move || {
            let _ = gtx.send_blocking(jobs::detect_gpu());
        });
        let a = app.clone();
        glib::spawn_future_local(async move {
            let gpu = grx.recv().await.ok().flatten();
            crate::log::write(&format!(
                "transcripts on: {}",
                gpu.as_deref().unwrap_or("the CPU")
            ));
            a.st.borrow_mut().gpu = Some(gpu);
            if a.st.borrow().config.transcribe {
                for dir in crate::transcribe::pending(&a.output_dir()) {
                    a.enqueue(dir);
                }
            }
            a.update_banner();
            a.pump();
        });
        app.settings.show_output_dir(&app.output_dir());
        app.show_hint();
        app.update_banner();
        app.update_footer();
        app.update_controls();
        window.present();
        Ok(app)
    }

    /// Ends the app: a recording is saved first; an unfinished transcript
    /// is resumed at the next start.
    fn quit(self: &Rc<Self>) {
        if self.st.borrow().phase != Phase::Idle {
            crate::log::write("quit requested while recording");
            self.st.borrow_mut().quit_after_stop = true;
            self.stop(); // Nothing to do while already saving.
            return;
        }
        self.cancel_job();
        self.window.destroy();
        self.gapp.quit();
    }

    fn setup_hotkey(self: &Rc<Self>) {
        let text = self.st.borrow().config.hotkey.clone();
        if text.trim().is_empty() {
            return;
        }
        let hk = match Hotkey::parse(&text) {
            Ok(h) => h,
            Err(e) => {
                self.set_status(&format!("Shortcut setting: {e}"), true);
                return;
            }
        };
        let label = hk.label.clone();
        let (tx, rx) = async_channel::unbounded::<()>();
        match crate::hotkey::listen(hk, move || {
            let _ = tx.send_blocking(());
        }) {
            Ok(()) => {
                self.st.borrow_mut().hotkey = Some(label);
                let a = self.clone();
                glib::spawn_future_local(async move {
                    while rx.recv().await.is_ok() {
                        a.toggle();
                    }
                });
            }
            Err(e) => {
                crate::log::write(&format!("global shortcut: {e:#}"));
                self.set_status(&format!("Global shortcut not available: {e:#}"), false);
            }
        }
    }

    /// The top bar icon (needs Ubuntu's AppIndicator extension, which is on
    /// by default; elsewhere nothing shows and nothing breaks).
    fn start_tray(self: &Rc<Self>) {
        // A NON_UNIQUE app has no bus connection of its own.
        let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(c) => c,
            Err(e) => {
                crate::log::write(&format!("tray icon: {e}"));
                return;
            }
        };
        // Not installed (a development build): stock icons.
        let installed = gdk::Display::default()
            .is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(APP_ID));
        let icons = if installed {
            crate::tray::Icons {
                idle: APP_ID.into(),
                recording: format!("{APP_ID}-recording"),
            }
        } else {
            crate::tray::Icons {
                idle: "audio-input-microphone".into(),
                recording: "media-record".into(),
            }
        };
        let (a, b, c) = (
            Rc::downgrade(self),
            Rc::downgrade(self),
            Rc::downgrade(self),
        );
        let actions = crate::tray::Actions {
            open: Box::new(move || {
                if let Some(a) = a.upgrade() {
                    a.window.present();
                }
            }),
            toggle: Box::new(move || {
                if let Some(a) = b.upgrade() {
                    a.toggle();
                }
            }),
            quit: Box::new(move || {
                if let Some(a) = c.upgrade() {
                    a.quit();
                }
            }),
        };
        match crate::tray::Tray::start(&conn, icons, actions) {
            Ok(t) => *self.tray.borrow_mut() = Some(t),
            Err(e) => crate::log::write(&format!("tray icon: {e}")),
        }
    }

    /// Adds or removes the login autostart entry to match the setting.
    fn sync_autostart(&self) {
        use recall_core::autostart;
        let exe = std::env::current_exe().unwrap_or_default();
        let enabled = self.st.borrow().config.autostart;
        if !recall_core::update::is_installed_package(&exe) {
            if enabled {
                crate::log::write(&format!(
                    "autostart: not changed, {} is not an installed package",
                    exe.display()
                ));
            }
            return;
        }
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let dir = autostart::dir(&self.paths.home, xdg);
        let config_dir = self
            .config_dir
            .as_ref()
            .map(|d| std::path::absolute(d).unwrap_or_else(|_| d.clone()));
        match autostart::sync(&dir, enabled, config_dir.as_deref()) {
            Ok(autostart::Action::Nothing) => {}
            Ok(a) => crate::log::write(&format!(
                "autostart: {} {}",
                if matches!(a, autostart::Action::Remove) {
                    "removed"
                } else {
                    "wrote"
                },
                dir.join(autostart::FILE).display()
            )),
            Err(e) => crate::log::write(&format!("autostart: {e:#}")),
        }
    }

    fn language(&self) -> Language {
        self.st.borrow().config.language
    }

    fn models_dir(&self) -> PathBuf {
        self.st.borrow().config.models_dir(&self.paths.data_home)
    }

    fn output_dir(&self) -> PathBuf {
        self.st.borrow().config.output_dir(&self.paths.home)
    }

    /// Changes the subtitle size by `step` points; 0 resets it.
    fn zoom(&self, step: f64) {
        let current = self.st.borrow().config.font_size;
        let size = if step == 0.0 {
            config::FONT_DEFAULT
        } else {
            current + step
        };
        self.set_font_size(size);
    }

    fn set_font_size(&self, size: f64) {
        let size = config::font_size(size);
        if self.st.borrow().config.font_size == size {
            return;
        }
        self.st.borrow_mut().config.font_size = size;
        self.apply_font_size(size);
        self.settings.show_font_size(size);
        self.save_config();
    }

    fn apply_font_size(&self, size: f64) {
        let size = config::font_size(size);
        #[allow(deprecated)]
        self.font_css
            .load_from_data(&format!(".subtitles {{ font-size: {size}pt; }}"));
    }

    fn save_config(&self) {
        let c = self.st.borrow().config.clone();
        if let Err(e) = c.save(&self.paths.config_file()) {
            crate::log::write(&format!("saving settings: {e:#}"));
        }
    }

    fn set_autostart(&self, enabled: bool) {
        if self.st.borrow().config.autostart == enabled {
            return;
        }
        self.st.borrow_mut().config.autostart = enabled;
        self.save_config();
        self.sync_autostart();
    }

    /// Asks for the folder new calls are saved in.
    fn choose_output_dir(self: &Rc<Self>) {
        #[allow(deprecated)]
        let chooser = gtk::FileChooserNative::new(
            Some("Save calls in"),
            Some(&self.window),
            gtk::FileChooserAction::SelectFolder,
            Some("Select"),
            None,
        );
        #[allow(deprecated)]
        {
            let current = self.output_dir();
            let _ = std::fs::create_dir_all(&current);
            let _ = chooser.set_current_folder(Some(&gio::File::for_path(&current)));
            let a = self.clone();
            chooser.connect_response(move |c, resp| {
                if resp == gtk::ResponseType::Accept
                    && let Some(dir) = c.file().and_then(|f| f.path())
                    && let Err(e) = a.set_output_dir(&dir)
                {
                    a.set_status(&e, true);
                }
                c.destroy();
            });
            chooser.show();
        }
    }

    /// New calls go to `dir` (not while recording).
    fn set_output_dir(self: &Rc<Self>, dir: &Path) -> Result<(), String> {
        if self.st.borrow().phase != Phase::Idle {
            return Err("cannot change the folder while recording".into());
        }
        if !dir.is_absolute() {
            return Err(format!("not an absolute path: {}", dir.display()));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot use {}: {e}", dir.display()))?;
        // The home folder is written as "~/…" so the setting survives a
        // renamed home; the default is written as "" (follows the default).
        let text = if dir == config::Config::default().output_dir(&self.paths.home) {
            String::new()
        } else {
            tilde(dir)
        };
        self.st.borrow_mut().config.output_dir = text;
        self.save_config();
        self.settings.show_output_dir(&self.output_dir());
        self.update_footer();
        if self.st.borrow().config.transcribe {
            for dir in crate::transcribe::pending(&self.output_dir()) {
                self.enqueue(dir);
            }
            self.pump();
        }
        Ok(())
    }

    fn set_language(self: &Rc<Self>, lang: Language) {
        if self.language() == lang {
            return;
        }
        self.st.borrow_mut().config.language = lang;
        self.save_config();
        self.update_banner();
        self.preload_model();
    }

    /// Loads the model in the background, so the first subtitles come quickly.
    fn preload_model(&self) {
        let (vosk, lang, dir) = (self.vosk.clone(), self.language(), self.models_dir());
        std::thread::spawn(move || {
            let _ = vosk.get(lang, &dir);
        });
    }

    fn toggle(self: &Rc<Self>) {
        let phase = self.st.borrow().phase;
        match phase {
            Phase::Idle => {
                if let Err(e) = self.start() {
                    self.set_status(&format!("{e:#}"), true);
                }
            }
            Phase::Recording => self.stop(),
            Phase::Stopping => {}
        }
    }

    fn start(self: &Rc<Self>) -> anyhow::Result<()> {
        let out = self.output_dir();
        std::fs::create_dir_all(&out)?;
        if let Some(free) = models::free_space(&out)
            && free < MIN_FREE_BYTES
        {
            anyhow::bail!(
                "Only {} free in {}: free some space before recording.",
                human_size(free),
                out.display()
            );
        }
        let (lang, config) = {
            let st = self.st.borrow();
            (st.config.language, st.config.clone())
        };
        let dir = call::new_dir(&out, chrono::Local::now(), lang);
        let opts = Options {
            dir: dir.clone(),
            language: lang,
            mic: Source::parse(&config.microphone),
            mic_channel: mic_channel::Mode::parse(&config.microphone_channel),
            others: Source::parse(&config.others),
            models_dir: self.models_dir(),
            vosk: self.vosk.clone(),
        };
        let recorder = Recorder::start(opts, self.events.clone())?;
        {
            let mut st = self.st.borrow_mut();
            st.recorder = Some(recorder);
            st.phase = Phase::Recording;
            st.started = Some(Instant::now());
            st.call_dir = Some(dir);
            st.segments.clear();
            st.partial = Default::default();
        }
        self.buffer.set_text("");
        self.pause_job(true);
        self.set_status("Recording…", false);
        self.update_controls();
        self.update_banner();
        self.update_footer();
        Ok(())
    }

    fn stop(self: &Rc<Self>) {
        let Some(recorder) = self.st.borrow_mut().recorder.take() else {
            return;
        };
        self.st.borrow_mut().phase = Phase::Stopping;
        self.update_controls();
        self.set_status("Saving…", false);
        let (tx, rx) = async_channel::bounded::<anyhow::Result<Finished>>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(recorder.stop());
        });
        let a = self.clone();
        glib::spawn_future_local(async move {
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("saving failed")));
            a.stopped(result);
        });
    }

    fn stopped(self: &Rc<Self>, result: anyhow::Result<Finished>) {
        {
            let mut st = self.st.borrow_mut();
            st.phase = Phase::Idle;
            st.started = None;
            st.partial = Default::default();
            st.levels = [f64::NEG_INFINITY; 2];
        }
        self.render_partials();
        for m in &self.meters {
            m.set_value(0.0);
        }
        if let Ok(f) = &result
            && self.st.borrow().config.transcribe
        {
            self.enqueue(f.dir.clone());
        }
        self.pause_job(false);
        match result {
            Ok(f) => self.set_status(
                &format!(
                    "Saved {} ({}, {} lines) in {}",
                    clock(f.duration),
                    if f.segments.is_empty() {
                        "no subtitles"
                    } else {
                        "subtitles"
                    },
                    f.segments.len(),
                    f.dir.display()
                ),
                false,
            ),
            Err(e) => self.set_status(&format!("Saving failed: {e:#}"), true),
        }
        self.update_controls();
        self.update_footer();
        crate::log::write(&format!(
            "stopped; quit={}",
            self.st.borrow().quit_after_stop
        ));
        if self.st.borrow().quit_after_stop {
            self.cancel_job();
            self.window.destroy();
            self.gapp.quit();
        }
    }

    fn on_event(self: &Rc<Self>, ev: Event) {
        if self.st.borrow().phase == Phase::Idle {
            return; // Late events of a finished recording.
        }
        match ev {
            Event::Level { speaker, db } => {
                self.st.borrow_mut().levels[idx(speaker)] = db;
                self.meters[idx(speaker)].set_value(levels::fraction(db));
            }
            Event::Partial { speaker, text } => {
                self.st.borrow_mut().partial[idx(speaker)] = text;
                self.render_partials();
            }
            Event::Segment(seg) => {
                self.append_segment(&seg);
                self.st.borrow_mut().segments.push(seg);
            }
            Event::Subtitles { speaker, status } => {
                crate::log::write(&format!("subtitles {speaker:?}: {status:?}"));
                match status {
                    SubtitleStatus::On => self.update_banner(),
                    SubtitleStatus::ModelMissing => self.update_banner(),
                    SubtitleStatus::Failed(e) => {
                        self.set_status(&format!("Subtitles off: {e}"), true)
                    }
                }
            }
            Event::Warning(w) => self.set_status(&w, false),
            Event::Error(e) => {
                self.set_status(&e, true);
                if self.st.borrow().phase == Phase::Recording {
                    self.stop();
                    // Keep the error visible over "Saving…".
                    self.set_status(&e, true);
                }
            }
        }
    }

    fn should_follow(&self) -> bool {
        let adj = self.view.vadjustment();
        adj.is_none_or(|a| a.value() + a.page_size() >= a.upper() - 40.0)
    }

    fn follow(&self) {
        self.view
            .scroll_to_mark(&self.end_mark, 0.0, false, 0.0, 1.0);
    }

    fn append_segment(&self, seg: &Segment) {
        let follow = self.should_follow();
        let b = &self.buffer;
        let mut it = b.iter_at_mark(&self.partial_mark);
        b.insert_with_tags_by_name(&mut it, &format!("{}  ", clock(seg.start)), &["time"]);
        let tag = seg.speaker.track_name();
        b.insert_with_tags_by_name(&mut it, &format!("{}: ", seg.speaker.label()), &[tag]);
        b.insert(&mut it, &format!("{}\n", seg.text));
        if follow {
            self.follow();
        }
    }

    fn render_partials(&self) {
        let follow = self.should_follow();
        let b = &self.buffer;
        let offset = b.iter_at_mark(&self.partial_mark).offset();
        let (mut start, mut end) = (b.iter_at_offset(offset), b.end_iter());
        b.delete(&mut start, &mut end);
        let st = self.st.borrow();
        for sp in Speaker::ALL {
            let text = &st.partial[idx(sp)];
            if !text.is_empty() {
                let mut it = b.end_iter();
                b.insert_with_tags_by_name(
                    &mut it,
                    &format!("{}: {text}…\n", sp.label()),
                    &["partial"],
                );
            }
        }
        b.move_mark(&self.partial_mark, &b.iter_at_offset(offset));
        if follow {
            self.follow();
        }
    }

    fn show_hint(&self) {
        let hk = self
            .st
            .borrow()
            .hotkey
            .clone()
            .map(|h| format!(" (or {h})"))
            .unwrap_or_default();
        self.buffer.set_text("");
        let mut it = self.buffer.end_iter();
        self.buffer.insert_with_tags_by_name(
            &mut it,
            &format!(
                "Join your call, choose its language and press Record{hk}.\n\
                 Your microphone is recorded as \"Me\", everything the computer plays as \"Others\".\n\
                 Wear headphones, or the others' voices also end up on your microphone."
            ),
            &["hint"],
        );
        let end = self.buffer.end_iter();
        self.buffer.move_mark(&self.partial_mark, &end);
    }

    fn update_controls(&self) {
        let phase = self.st.borrow().phase;
        let (label, sensitive) = match phase {
            Phase::Idle => ("● Record", true),
            Phase::Recording => ("■ Stop", true),
            Phase::Stopping => ("Saving…", false),
        };
        self.record.set_label(label);
        self.record.set_sensitive(sensitive);
        if phase == Phase::Idle {
            self.record.remove_css_class("destructive-action");
            self.record.add_css_class("suggested-action");
        } else {
            self.record.remove_css_class("suggested-action");
            self.record.add_css_class("destructive-action");
        }
        self.language.set_sensitive(phase == Phase::Idle);
        self.settings.folder.set_sensitive(phase == Phase::Idle);
        if let Some(t) = self.tray.borrow().as_ref() {
            t.set_recording(phase != Phase::Idle);
        }
        if phase != Phase::Recording {
            self.elapsed.add_css_class("dim-label");
        } else {
            self.elapsed.remove_css_class("dim-label");
        }
    }

    /// Models needed now: live subtitles for the language, and (when
    /// transcripts are on and the graphics card is known) Whisper + VAD.
    fn needed_models(&self) -> Vec<&'static ModelInfo> {
        let st = self.st.borrow();
        let mut v = vec![st.config.language.live_model()];
        if st.config.transcribe
            && let Some(gpu) = &st.gpu
        {
            let use_gpu = st.config.gpu && gpu.is_some();
            v.push(st.config.transcript_model.resolve(use_gpu).info());
            v.push(&whisper::VAD_MODEL);
        }
        v
    }

    fn update_banner(&self) {
        let missing = models::missing(&self.models_dir(), &self.needed_models());
        let downloading = self.st.borrow().downloading;
        self.banner.set_visible(!missing.is_empty() || downloading);
        if downloading || missing.is_empty() {
            return;
        }
        let lang = self.language();
        let parts: Vec<String> = missing
            .iter()
            .map(|m| {
                let what = if *m == lang.live_model() {
                    format!("live subtitles in {}", lang.name())
                } else if *m == &whisper::VAD_MODEL {
                    "voice detection".to_string()
                } else {
                    "the accurate transcript after the call".to_string()
                };
                format!("{what} ({})", m.size_label())
            })
            .collect();
        let total: u64 = missing.iter().map(|m| m.size).sum();
        self.banner_label.set_label(&format!(
            "Speech models needed (downloaded once): {}.{}",
            parts.join(", "),
            if self.st.borrow().phase == Phase::Recording {
                " Recording works without them."
            } else {
                ""
            }
        ));
        self.banner_button
            .set_label(&format!("Download {}", human_size(total)));
        self.banner_button.set_sensitive(true);
        self.banner_progress.set_visible(false);
    }

    fn download_model(self: &Rc<Self>) {
        if self.st.borrow().downloading {
            return;
        }
        let dir = self.models_dir();
        let missing = models::missing(&dir, &self.needed_models());
        if missing.is_empty() {
            return;
        }
        self.st.borrow_mut().downloading = true;
        self.banner_button.set_sensitive(false);
        self.banner_progress.set_visible(true);
        self.banner_progress.set_fraction(0.0);
        self.banner_label.set_label("Downloading speech models…");
        enum Msg {
            Progress(f64),
            Done(anyhow::Result<()>),
        }
        let (tx, rx) = async_channel::unbounded::<Msg>();
        std::thread::spawn(move || {
            let cancel = AtomicBool::new(false);
            let total: u64 = missing.iter().map(|m| m.size).sum::<u64>().max(1);
            let mut before = 0u64;
            let mut last = 0.0;
            let r = missing.iter().try_for_each(|m| {
                models::download(
                    m,
                    &dir,
                    &mut |done, _| {
                        let f = (before + done) as f64 / total as f64;
                        if f - last >= 0.005 {
                            last = f;
                            let _ = tx.send_blocking(Msg::Progress(f));
                        }
                    },
                    &cancel,
                )?;
                before += m.size;
                Ok(())
            });
            let _ = tx.send_blocking(Msg::Done(r));
        });
        let a = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    Msg::Progress(f) => a.banner_progress.set_fraction(f),
                    Msg::Done(r) => {
                        a.st.borrow_mut().downloading = false;
                        match r {
                            Ok(()) => {
                                a.set_status("Speech models installed.", false);
                                a.preload_model();
                                a.pump();
                            }
                            Err(e) => a.set_status(&format!("Download failed: {e:#}"), true),
                        }
                        a.update_banner();
                        a.update_footer();
                    }
                }
            }
        });
    }

    // --- Transcripts -----------------------------------------------------

    fn enqueue(self: &Rc<Self>, dir: PathBuf) {
        {
            let mut st = self.st.borrow_mut();
            let running = st.running.as_ref().is_some_and(|r| r.dir == dir);
            if running || st.queue.contains(&dir) {
                return;
            }
            st.queue.push_back(dir);
        }
        self.pump();
    }

    /// Starts the next transcription if nothing runs, nothing is being
    /// recorded and the models are there.
    fn pump(self: &Rc<Self>) {
        let (dir, use_gpu) = {
            let st = self.st.borrow();
            if st.running.is_some() || st.phase != Phase::Idle || st.queue.is_empty() {
                return;
            }
            let Some(gpu) = &st.gpu else { return };
            let dir = st.queue.front().unwrap().clone();
            let use_gpu = st.config.gpu && gpu.is_some() && !st.cpu_only.contains(&dir);
            (dir, use_gpu)
        };
        let model = self.st.borrow().config.transcript_model.resolve(use_gpu);
        if models::missing(&self.models_dir(), &[model.info(), &whisper::VAD_MODEL])
            .iter()
            .any(|m| m.name == model.info().name)
        {
            self.update_banner();
            self.show_job(
                &format!("Transcript waiting for its model: {}", folder_name(&dir)),
                None,
                false,
            );
            return;
        }
        self.st.borrow_mut().queue.pop_front();
        match jobs::spawn(
            &dir,
            self.config_dir.as_deref(),
            use_gpu,
            self.jobs_tx.clone(),
        ) {
            Ok(r) => {
                let mut st = self.st.borrow_mut();
                st.running = Some(r);
                st.job_progress = 0.0;
                drop(st);
                self.show_job(
                    &format!("Transcribing {}…", folder_name(&dir)),
                    Some(0.0),
                    false,
                );
            }
            Err(e) => self.set_status(&format!("Cannot start the transcriber: {e:#}"), true),
        }
    }

    /// SIGSTOP/SIGCONT the transcriber (recording needs the computer).
    fn pause_job(self: &Rc<Self>, pause: bool) {
        let pid = self.st.borrow().running.as_ref().map(|r| r.pid);
        if let Some(pid) = pid {
            jobs::signal(pid, if pause { libc::SIGSTOP } else { libc::SIGCONT });
            if pause {
                self.job_label
                    .set_label("Transcript paused while recording");
            }
        }
        if !pause {
            self.pump();
        }
    }

    fn cancel_job(&self) {
        if let Some(r) = self.st.borrow_mut().running.take() {
            jobs::signal(r.pid, libc::SIGKILL);
        }
    }

    fn on_job_event(self: &Rc<Self>, ev: JobEvent) {
        match ev {
            JobEvent::Progress { dir, fraction } => {
                self.st.borrow_mut().job_progress = fraction;
                if self.st.borrow().phase == Phase::Idle {
                    self.show_job(
                        &format!(
                            "Transcribing {}… {:.0}%",
                            folder_name(&dir),
                            fraction * 100.0
                        ),
                        Some(fraction),
                        false,
                    );
                }
            }
            JobEvent::Done {
                dir,
                result,
                crashed,
                gpu,
            } => {
                let was_ours = self
                    .st
                    .borrow()
                    .running
                    .as_ref()
                    .is_some_and(|r| r.dir == dir);
                if !was_ours {
                    return; // Cancelled.
                }
                self.st.borrow_mut().running = None;
                match result {
                    Ok(md) => {
                        self.st.borrow_mut().last_transcript = Some(md);
                        self.show_job(
                            &format!("Transcript ready: {}", folder_name(&dir)),
                            None,
                            true,
                        );
                    }
                    Err(_) if crashed && gpu => {
                        crate::log::write("transcriber crashed on the GPU; retrying on the CPU");
                        let mut st = self.st.borrow_mut();
                        st.cpu_only.insert(dir.clone());
                        st.queue.push_front(dir);
                    }
                    Err(e) => {
                        self.show_job(
                            &format!("Transcript of {} failed: {e}", folder_name(&dir)),
                            None,
                            false,
                        );
                    }
                }
                self.pump();
            }
        }
    }

    fn show_job(&self, text: &str, progress: Option<f64>, can_open: bool) {
        self.job_row.set_visible(true);
        self.job_label.set_label(text);
        self.job_bar.set_visible(progress.is_some());
        if let Some(p) = progress {
            self.job_bar.set_fraction(p);
        }
        self.job_open.set_visible(can_open);
    }

    fn open_path(&self, path: &Path) {
        let uri = gio::File::for_path(path).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            self.set_status(&format!("Cannot open {}: {e}", path.display()), true);
        }
    }

    /// The folder all calls are saved in.
    fn open_folder(&self) {
        let dir = self.output_dir();
        let _ = std::fs::create_dir_all(&dir);
        self.open_path(&dir);
    }

    fn set_status(&self, text: &str, error: bool) {
        {
            let mut st = self.st.borrow_mut();
            st.status = text.to_string();
            st.status_is_error = error;
        }
        crate::log::write(&format!("status: {text}"));
        self.status.set_label(text);
        if error {
            self.status.add_css_class("status-error");
        } else {
            self.status.remove_css_class("status-error");
        }
    }

    fn update_footer(&self) {
        let out = self.output_dir();
        let free = models::free_space(&out)
            .map(|f| format!(" · {} free", human_size(f)))
            .unwrap_or_default();
        let hk = self
            .st
            .borrow()
            .hotkey
            .clone()
            .map(|h| format!(" · {h} starts/stops"))
            .unwrap_or_default();
        self.footer
            .set_label(&format!("Calls are saved in {}{free}{hk}", tilde(&out)));
    }

    fn tick(self: &Rc<Self>) {
        let (started, phase) = {
            let st = self.st.borrow();
            (st.started, st.phase)
        };
        if let (Phase::Recording, Some(t)) = (phase, started) {
            let secs = t.elapsed().as_secs();
            self.elapsed.set_label(&clock(secs as f64));
            if secs % 30 == 0 {
                self.update_footer();
                if let Some(free) = models::free_space(&self.output_dir())
                    && free < MIN_FREE_BYTES / 2
                {
                    self.set_status(
                        &format!("Disk almost full ({} free): stopping.", human_size(free)),
                        true,
                    );
                    self.stop();
                }
            }
        }
    }

    fn state(&self) -> State {
        let st = self.st.borrow();
        State {
            recording: st.phase == Phase::Recording,
            language: st.config.language,
            elapsed: st.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0),
            call_dir: st.call_dir.as_ref().map(|d| d.display().to_string()),
            segments: st.segments.clone(),
            partial: Speaker::ALL
                .into_iter()
                .filter(|s| !st.partial[idx(*s)].is_empty())
                .map(|s| (s, st.partial[idx(s)].clone()))
                .collect(),
            levels: Speaker::ALL
                .into_iter()
                .map(|s| (s, st.levels[idx(s)]))
                .collect(),
            status: st.status.clone(),
            font_size: st.config.font_size,
            transcribing: st.running.as_ref().map(|r| r.dir.display().to_string()),
            transcript_progress: st.job_progress,
            transcript_queue: st.queue.len(),
            last_transcript: st.last_transcript.as_ref().map(|p| p.display().to_string()),
            update: self.updater.status(),
            window_visible: self.window.is_visible(),
            output_dir: self.output_dir().display().to_string(),
            autostart: st.config.autostart,
        }
    }

    fn handle_control(self: &Rc<Self>, cmd: Command) -> Reply {
        let phase = self.st.borrow().phase;
        match cmd {
            Command::State => return Reply::state(self.state()),
            Command::Start if phase != Phase::Idle => return Reply::error("already recording"),
            Command::Start => {
                if let Err(e) = self.start() {
                    self.set_status(&format!("{e:#}"), true);
                    return Reply::error(format!("{e:#}"));
                }
            }
            Command::Stop if phase != Phase::Recording => return Reply::error("not recording"),
            Command::Stop => self.stop(),
            Command::Toggle => self.toggle(),
            Command::Language { language } => {
                if phase != Phase::Idle {
                    return Reply::error("cannot change the language while recording");
                }
                let pos = Language::ALL
                    .iter()
                    .position(|l| *l == language)
                    .unwrap_or(0);
                self.language.set_selected(pos as u32);
            }
            Command::FontSize { size } => self.set_font_size(size),
            Command::OutputDir { dir } => {
                if let Err(e) = self.set_output_dir(Path::new(&dir)) {
                    return Reply::error(e);
                }
            }
            Command::Autostart { enabled } => self.settings.autostart.set_active(enabled),
            Command::Transcribe { dir } => {
                let dir = dir
                    .map(PathBuf::from)
                    .or_else(|| self.st.borrow().call_dir.clone());
                let Some(dir) = dir.filter(|d| d.join(call::INFO_FILE).exists()) else {
                    return Reply::error("no such call folder");
                };
                let _ = std::fs::remove_file(dir.join(call::TRANSCRIPT_FAILED));
                self.enqueue(dir);
            }
            Command::CheckUpdate => self.updater.check(true),
            Command::Quit => self.quit(),
            Command::Close => {
                self.window.close();
            }
            Command::Show => self.window.present(),
        }
        Reply::state(self.state())
    }
}

/// The settings popover behind the gear button in the title bar.
struct Settings {
    button: gtk::MenuButton,
    language: gtk::DropDown,
    smaller: gtk::Button,
    bigger: gtk::Button,
    font_size: gtk::Label,
    folder: gtk::Button,
    folder_label: gtk::Label,
    open: gtk::Button,
    autostart: gtk::Switch,
}

impl Settings {
    fn new(config: &Config) -> Settings {
        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        let mut row = 0;
        let mut add = |title: &str, widget: &gtk::Widget| {
            let label = gtk::Label::new(Some(title));
            label.set_xalign(0.0);
            grid.attach(&label, 0, row, 1, 1);
            widget.set_halign(gtk::Align::End);
            grid.attach(widget, 1, row, 1, 1);
            row += 1;
        };

        let names: Vec<&str> = Language::ALL.iter().map(|l| l.name()).collect();
        let language = gtk::DropDown::from_strings(&names);
        language.set_tooltip_text(Some("Language of the call"));
        let pos = Language::ALL
            .iter()
            .position(|l| *l == config.language)
            .unwrap_or(0);
        language.set_selected(pos as u32);
        add("Language", language.upcast_ref());

        // Also Ctrl+minus / Ctrl+plus / Ctrl+0 and Ctrl+mouse wheel over
        // the subtitles.
        let zoom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom.add_css_class("linked");
        let smaller = gtk::Button::from_icon_name("zoom-out-symbolic");
        smaller.set_tooltip_text(Some("Smaller subtitles (Ctrl+minus)"));
        let font_size = gtk::Label::new(None);
        font_size.set_width_chars(6);
        let bigger = gtk::Button::from_icon_name("zoom-in-symbolic");
        bigger.set_tooltip_text(Some("Bigger subtitles (Ctrl+plus)"));
        zoom.append(&smaller);
        zoom.append(&font_size);
        zoom.append(&bigger);
        add("Subtitle size", zoom.upcast_ref());

        let folder_label = gtk::Label::new(None);
        folder_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        folder_label.set_max_width_chars(28);
        let folder = gtk::Button::builder().child(&folder_label).build();
        folder.set_tooltip_text(Some("Choose where new calls are saved"));
        add("Save calls in", folder.upcast_ref());

        let autostart = gtk::Switch::new();
        autostart.set_active(config.autostart);
        autostart.set_tooltip_text(Some("Start recall when you log in"));
        add("Start at login", autostart.upcast_ref());

        let open = gtk::Button::with_label("Open recordings folder");
        open.set_margin_top(4);
        grid.attach(&open, 0, row, 2, 1);

        let popover = gtk::Popover::new();
        popover.set_child(Some(&grid));
        let button = gtk::MenuButton::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Settings")
            .popover(&popover)
            .build();
        let s = Settings {
            button,
            language,
            smaller,
            bigger,
            font_size,
            folder,
            folder_label,
            open,
            autostart,
        };
        s.show_font_size(config::font_size(config.font_size));
        s
    }

    fn show_font_size(&self, size: f64) {
        self.font_size.set_text(&format!("{size} pt"));
    }

    fn show_output_dir(&self, dir: &Path) {
        self.folder_label.set_text(&tilde(dir));
        self.folder.set_tooltip_text(Some(&format!(
            "{}\nChoose where new calls are saved",
            dir.display()
        )));
    }
}

/// The call folder's name ("2026-11-02 14-30 (en)").
fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// "~/Recordings/Calls" for paths in the home folder.
fn tilde(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if p.starts_with(&home) && home != Path::new("/") => {
            format!("~/{}", p.strip_prefix(&home).unwrap().display())
        }
        _ => p.display().to_string(),
    }
}
