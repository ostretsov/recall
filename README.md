# recall

Records your calls on Ubuntu (Google Meet now; Zoom and Teams work the same
way) and shows live subtitles of who says what, in **English, Russian or
Spanish**. Everything runs on your computer: no account, no cloud.

- **Two tracks, so the speaker is always known.** Your microphone is
  recorded as **Me**, everything the computer plays (the other people in the
  call) as **Others**. Wear headphones, or the others' voices also reach
  your microphone and get labelled "Me".
- **Live subtitles** with [Vosk](https://alphacephei.com/vosk/) small models
  (~40–50 MB per language, downloaded once from inside the app). They take
  little CPU, so it runs on older laptops.
- **Accurate transcript after the call** with
  [whisper.cpp](https://github.com/ggml-org/whisper.cpp): Markdown to read,
  JSON for tools. It runs in the background, on a dedicated graphics card
  through Vulkan when there is one, and pauses while you record the next call.
- **Small files:** Opus at 32 kb/s per track, about 30 MB per hour of call.
- **Start/stop** with the button or the global shortcut **Ctrl+Alt+R** (X11).

Planned: video recording of the call window.

## Install

Download `recall_0.2.0_amd64.deb` from the
[releases](../../releases) and install it:

```sh
sudo apt install ./recall_0.2.0_amd64.deb
```

It is self-contained (GTK 4, GStreamer, Vosk and whisper.cpp are bundled
in `/opt/recall`) and works on Ubuntu 22.04 and newer. Start **recall** from
the app menu. The first time, press **Download** to get the speech models
(about 250 MB without a graphics card, 620 MB with one).

## Using it

1. Join the call, pick the call's language, press **● Record** (or Ctrl+Alt+R).
2. Subtitles appear as people talk: **Me** in blue, **Others** in green; the
   grey italic line is still being recognized.
3. Press **■ Stop**. The accurate transcript is made in the background;
   **Open folder** (the call's folder: transcript and recordings) appears
   when it's ready.

While recall runs, its icon is in the top bar (Ubuntu's AppIndicator
extension, on by default): click it to show the window; its menu starts or
stops recording and quits. The icon turns red while recording. Closing the
window only hides it: recall keeps running (recording, transcripts, the
shortcut) until you choose **Quit** in the icon's menu. Without the icon
(the extension turned off) closing the window quits.

Settings (the gear button in the title bar): the call's language, subtitle
size (also Ctrl+minus / Ctrl+plus, Ctrl+0 resets, or Ctrl+mouse wheel over
the subtitles), the folder calls are saved in, starting at login, and a
button that opens the recordings folder.

Each call is a folder in `~/Recordings/Calls/`:

| File          | What                                                      |
|---------------|-----------------------------------------------------------|
| `me.opus`     | your microphone                                           |
| `others.opus` | what the computer played                                  |
| `live.md`     | the live subtitles as a readable transcript              |
| `live.jsonl`  | the same, one utterance per line with word timings        |
| `transcript.md`   | the accurate transcript (Whisper), made after the call |
| `transcript.json` | the same with start/end times per line                 |
| `call.json`   | start time, duration, language                            |

The files are written while recording, so a crash loses at most the last
moment.

## Settings

`~/.config/recall/config.toml` (all optional; the app saves the language
itself):

```toml
language = "ru"                  # en, ru, es
output_dir = "~/Recordings/Calls"
microphone = "default"           # or a PulseAudio source name, "none"
microphone_channel = "auto"      # auto, both, left, right (Scarlett-type interfaces)
others = "default"               # monitor of the default output, or a source name, "none"
hotkey = "Ctrl+Alt+R"            # "" for none
models_dir = ""                  # default ~/.local/share/recall/models
font_size = 13.0                 # subtitle size in points
transcribe = true                # accurate transcript after each call
transcript_model = "auto"        # auto, base, small, turbo
gpu = true                       # use a dedicated graphics card (Vulkan)
check_updates = true             # look for a newer release at start
autostart = false                # start recall when you log in
```

**Updates.** At start the installed app asks GitHub whether a newer release
exists. If so, a bar at the top offers to install it: one click downloads
the `.deb`, checks its SHA-256 and installs it (Ubuntu asks for your
password), then **Restart recall** starts the new version (not while
recording; an unfinished transcript is resumed). Development builds don't
check. `check_updates = false` turns it off.

**Start at login.** `autostart = true` makes the installed app add
`~/.config/autostart/recall.desktop` at its next start (`false` removes it
again). The entry starts recall with `--hidden`: only the top bar icon
shows (if there is no top bar icon after 15 seconds, the window opens
anyway). With `--config-dir` the entry starts recall with that folder.

### Transcript models

`auto` picks **turbo** (Whisper large-v3-turbo, 574 MB) when there is a
dedicated graphics card and **small** (190 MB) otherwise. Measured on a
laptop with an i5-7300HQ and a GeForce GTX 1050, per hour of call:

| Model | Graphics card | CPU only |
|-------|---------------|----------|
| base  | ~7 min        | ~9 min   |
| small | ~12 min       | ~26 min (all cores busy) |
| turbo | ~24 min       | too slow |

Integrated Intel graphics are not used (slower than the CPU here, and their
Vulkan driver crashed in tests). If a graphics driver crashes anyway, the
call is transcribed again on the CPU. A call whose transcript was
interrupted (app closed) is resumed at the next start.

`pactl list sources short` lists source names; `.monitor` sources are what an
output device plays. For testing, `microphone`/`others` also take `test` (a
quiet tone) and `file:/path/to/audio.wav` (played in real time).

## Known limitations

- "Others" is the output device that was the default **when you pressed
  Record**. Switch to your headphones before you start; a switch during the
  call is not followed yet.
- The global shortcut needs an X11 session (Ubuntu on Wayland has no API for
  it); the button always works.
- Small Vosk models make mistakes, especially with names and fast speech.
  The after-call transcript (planned) is for accuracy.

## Problems

`~/.cache/recall/recall.log` has what happened (previous run:
`recall.prev.log`); the transcriber writes `transcribe.log`. A failed
transcript leaves `transcript.failed` with the reason in the call folder. `recall --self-test` checks the installation.

## Building

```sh
make deps        # once: build packages (sudo) + Vosk and Vulkan SDK parts into vendor/
make run         # debug build with your settings
make demo        # test tones instead of your microphone/output
make test lint   # before committing
make deb         # target/recall_<version>_amd64.deb
make docker-test # install the .deb in clean Ubuntu 22.04/24.04/rolling containers
```

From the command line: `recall --download-model ru` (also `base`, `small`,
`turbo`, `vad`), `recall --transcribe CALL_FOLDER` (`--no-gpu` for the CPU),
`recall --gpu` (which graphics card would be used).

### Control API

For scripts and tests: `recall --control SOCK` listens on a Unix socket;
`recall --ctl SOCK JSON` sends one command and prints the reply.

```sh
recall --ctl /tmp/r.sock '{"cmd":"start"}'
recall --ctl /tmp/r.sock '{"cmd":"state"}'     # segments, partial text, levels, status
recall --ctl /tmp/r.sock '{"cmd":"language","language":"es"}'
recall --ctl /tmp/r.sock '{"cmd":"font_size","size":18}'
recall --ctl /tmp/r.sock '{"cmd":"output_dir","dir":"/abs/path"}'
recall --ctl /tmp/r.sock '{"cmd":"autostart","enabled":true}'
recall --ctl /tmp/r.sock '{"cmd":"transcribe"}'  # (again) for the last call, or "dir":"…"
recall --ctl /tmp/r.sock '{"cmd":"check_update"}' # look for a newer release now; the state's "update" is the bar's text
recall --ctl /tmp/r.sock '{"cmd":"close"}'     # the window's close button; "show" brings it back
recall --ctl /tmp/r.sock '{"cmd":"stop"}'      # also: toggle, quit
```

### Release

```sh
make version V=0.2.0 && git commit -am "Release 0.2.0" && git tag v0.2.0 && git push --follow-tags
```

The `release` workflow builds the `.deb` on Ubuntu 22.04, tests it in clean
containers and attaches it to a GitHub release. The `ci` workflow runs lint
and tests on every push.

## License

MIT. Vosk and its models (Apache 2.0) come from alphacephei.com;
whisper.cpp (MIT) is built in; the Whisper models (MIT) and the Silero VAD
model (MIT) are downloaded from Hugging Face.
