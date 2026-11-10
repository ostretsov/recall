//! PulseAudio/PipeWire device lookup (through `pactl`, which works with both).

use std::process::Command;

use recall_core::pipelines::Source;

/// Name of the default output device (sink).
pub fn default_sink() -> Option<String> {
    let out = Command::new("pactl").arg("info").output().ok()?;
    parse_default_sink(&String::from_utf8_lossy(&out.stdout))
}

fn parse_default_sink(info: &str) -> Option<String> {
    info.lines()
        .find_map(|l| l.strip_prefix("Default Sink:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The concrete source for the "others" track: "default" becomes the monitor
/// of the current default output (what the call sounds like).
pub fn resolve_others(source: Source) -> anyhow::Result<Source> {
    match source {
        Source::Default => default_sink()
            .map(|sink| Source::Pulse(format!("{sink}.monitor")))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot find the sound output device (is PulseAudio/PipeWire running?)"
                )
            }),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl_info() {
        let info = "Server Name: PulseAudio (on PipeWire 0.3.48)\n\
                    Default Sink: alsa_output.usb-Focusrite.analog-surround-40\n\
                    Default Source: alsa_input.usb-Focusrite.analog-stereo\n";
        assert_eq!(
            parse_default_sink(info).as_deref(),
            Some("alsa_output.usb-Focusrite.analog-surround-40")
        );
        assert_eq!(parse_default_sink("Default Sink: \n"), None);
        assert_eq!(parse_default_sink(""), None);
    }

    #[test]
    fn explicit_sources_stay() {
        assert_eq!(resolve_others(Source::Test).unwrap(), Source::Test);
        let p = Source::Pulse("x.monitor".into());
        assert_eq!(resolve_others(p.clone()).unwrap(), p);
    }
}
