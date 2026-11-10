//! Runtime dependency check: the GStreamer elements recall uses and the
//! Ubuntu packages that provide them.  The .deb bundles all of them; this is
//! for development builds and `--self-test`.

/// (GStreamer element, Ubuntu package providing it).
pub const ELEMENTS: &[(&str, &str)] = &[
    ("audioconvert", "gstreamer1.0-plugins-base"),
    ("audioresample", "gstreamer1.0-plugins-base"),
    ("audiotestsrc", "gstreamer1.0-plugins-base"),
    ("opusenc", "gstreamer1.0-plugins-base"),
    ("opusdec", "gstreamer1.0-plugins-base"),
    ("oggmux", "gstreamer1.0-plugins-base"),
    ("oggdemux", "gstreamer1.0-plugins-base"),
    ("appsink", "gstreamer1.0-plugins-base"),
    ("decodebin", "gstreamer1.0-plugins-base"),
    ("wavparse", "gstreamer1.0-plugins-good"),
    ("pulsesrc", "gstreamer1.0-pulseaudio"),
];

/// (program, Ubuntu package).
pub const PROGRAMS: &[(&str, &str)] = &[("pactl", "pulseaudio-utils")];

/// Packages that need to be installed, sorted and without duplicates.
pub fn missing_packages(
    has_element: impl Fn(&str) -> bool,
    has_program: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut pkgs: Vec<&str> = ELEMENTS
        .iter()
        .filter(|(e, _)| !has_element(e))
        .map(|(_, p)| *p)
        .chain(
            PROGRAMS
                .iter()
                .filter(|(b, _)| !has_program(b))
                .map(|(_, p)| *p),
        )
        .collect();
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_missing() {
        assert!(missing_packages(|_| true, |_| true).is_empty());
    }

    #[test]
    fn deduplicated_packages() {
        let m = missing_packages(|e| e != "opusenc" && e != "oggmux", |_| false);
        assert_eq!(m, vec!["gstreamer1.0-plugins-base", "pulseaudio-utils"]);
    }
}
