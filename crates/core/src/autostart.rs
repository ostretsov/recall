//! Start recall at login (`autostart = true`): an XDG autostart entry,
//! `~/.config/autostart/recall.desktop`.
//!
//! The app writes or removes the entry at start to match the setting.  Only
//! installed packages do (a development build must not put itself into the
//! user's session), and the entry is removed only if it is the one this
//! instance would write, so an instance with another `--config-dir` leaves
//! the user's entry alone.
//!
//! The entry starts recall with `--hidden`: only the top bar icon, no window.

use std::path::{Path, PathBuf};

/// File name of the entry in the autostart folder.
pub const FILE: &str = "recall.desktop";
/// The installed launcher (`/usr/bin/recall` → `/opt/recall/bin/recall`).
pub const EXEC: &str = "/usr/bin/recall";

/// `$XDG_CONFIG_HOME/autostart` (`~/.config/autostart`).
pub fn dir(home: &Path, xdg_config_home: Option<PathBuf>) -> PathBuf {
    xdg_config_home
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("autostart")
}

/// Command-line flag: start without showing the window.
pub const HIDDEN: &str = "--hidden";

/// The entry's contents; `config_dir` is the `--config-dir` the app was
/// started with, if any.
pub fn desktop_entry(config_dir: Option<&Path>) -> String {
    entry(config_dir, true)
}

/// The entry written by 0.2.0 (window shown at login); still ours to
/// replace or remove.
fn old_entry(config_dir: Option<&Path>) -> String {
    entry(config_dir, false)
}

fn entry(config_dir: Option<&Path>, hidden: bool) -> String {
    let mut exec = EXEC.to_string();
    if let Some(d) = config_dir {
        exec.push_str(" --config-dir ");
        exec.push_str(&quote(&d.display().to_string()));
    }
    if hidden {
        exec.push(' ');
        exec.push_str(HIDDEN);
    }
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=recall\n\
         Comment=Record calls with live subtitles\n\
         Exec={exec}\n\
         Icon=io.github.ostretsov.recall\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Quotes an argument for a desktop entry's `Exec` key when needed.
fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@~".contains(c))
    {
        return arg.to_string();
    }
    let mut q = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    // In a .desktop value a backslash is itself escaped.
    q.replace('\\', "\\\\")
}

/// What to do with the entry file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Write(String),
    Remove,
}

/// Brings the entry in line with the setting.  `existing`: the file's
/// current contents, if it exists.
pub fn plan(enabled: bool, config_dir: Option<&Path>, existing: Option<&str>) -> Action {
    let want = desktop_entry(config_dir);
    let ours = |e: &str| e == want || e == old_entry(config_dir);
    match (enabled, existing) {
        (true, Some(e)) if e == want => Action::Nothing,
        (true, _) => Action::Write(want),
        (false, Some(e)) if ours(e) => Action::Remove,
        (false, _) => Action::Nothing,
    }
}

/// Applies [`plan`] to `dir/recall.desktop`; returns what was done.
pub fn sync(dir: &Path, enabled: bool, config_dir: Option<&Path>) -> anyhow::Result<Action> {
    let path = dir.join(FILE);
    let existing = std::fs::read_to_string(&path).ok();
    let action = plan(enabled, config_dir, existing.as_deref());
    match &action {
        Action::Nothing => {}
        Action::Write(text) => {
            std::fs::create_dir_all(dir)?;
            let tmp = path.with_extension("desktop.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)?;
        }
        Action::Remove => std::fs::remove_file(&path)?,
    }
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_dir() {
        assert_eq!(
            dir(Path::new("/h"), None),
            Path::new("/h/.config/autostart")
        );
        assert_eq!(
            dir(Path::new("/h"), Some("/x".into())),
            Path::new("/x/autostart")
        );
        assert_eq!(
            dir(Path::new("/h"), Some("rel".into())),
            Path::new("/h/.config/autostart")
        );
    }

    #[test]
    fn entry() {
        let e = desktop_entry(None);
        assert!(e.starts_with("[Desktop Entry]\n"));
        assert!(e.contains("\nExec=/usr/bin/recall --hidden\n"));
        let e = desktop_entry(Some(Path::new("/home/a/.config/recall-work")));
        assert!(e.contains(
            "\nExec=/usr/bin/recall --config-dir /home/a/.config/recall-work --hidden\n"
        ));
        let e = desktop_entry(Some(Path::new("/home/a/my calls")));
        assert!(
            e.contains("Exec=/usr/bin/recall --config-dir \"/home/a/my calls\" --hidden\n"),
            "{e}"
        );
    }

    #[test]
    fn plans() {
        let ours = desktop_entry(None);
        assert_eq!(plan(true, None, None), Action::Write(ours.clone()));
        assert_eq!(plan(true, None, Some(&ours)), Action::Nothing);
        assert_eq!(plan(true, None, Some("old")), Action::Write(ours.clone()));
        assert_eq!(plan(false, None, Some(&ours)), Action::Remove);
        assert_eq!(plan(false, None, None), Action::Nothing);
        // Someone else's entry (another --config-dir, or edited by hand).
        assert_eq!(
            plan(false, Some(Path::new("/t")), Some(&ours)),
            Action::Nothing
        );
        assert_eq!(plan(false, None, Some("edited")), Action::Nothing);
        // The entry of 0.2.0 (no --hidden) is upgraded or removed.
        let old = old_entry(None);
        assert!(old.contains("\nExec=/usr/bin/recall\n"));
        assert_eq!(plan(true, None, Some(&old)), Action::Write(ours.clone()));
        assert_eq!(plan(false, None, Some(&old)), Action::Remove);
    }

    #[test]
    fn sync_writes_and_removes() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("autostart");
        assert!(matches!(sync(&dir, true, None).unwrap(), Action::Write(_)));
        assert!(dir.join(FILE).exists());
        assert_eq!(sync(&dir, true, None).unwrap(), Action::Nothing);
        assert_eq!(sync(&dir, false, None).unwrap(), Action::Remove);
        assert!(!dir.join(FILE).exists());
        assert_eq!(sync(&dir, false, None).unwrap(), Action::Nothing);
    }
}
