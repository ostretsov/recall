# Notes for coding agents

recall: call recorder with live subtitles for Ubuntu (Rust, GTK 4, GStreamer,
Vosk). `README.md` describes the features; this file is about working on the
code.

## Layout

- `crates/core` — logic testable without a display, devices or models:
  settings, languages + model list (URLs, sha256), transcript segments and
  Markdown, GStreamer pipeline *descriptions*, mic channel pick, hotkey
  parsing, control API types, call folder layout, Whisper models + text
  cleanup (`whisper.rs`), speech packing and time mapping (`speech.rs`). New logic goes here, with
  unit tests.
- `crates/app` — GTK window (`ui.rs`), recording + live recognition threads
  (`recorder.rs`), model download/cache (`models.rs`), X11 global shortcut
  (`hotkey.rs`), control socket (`control.rs`), `pactl` lookups
  (`devices.rs`), `--self-test`, Whisper transcription (`transcribe.rs`,
  runs in the child process `recall --transcribe DIR --machine`) and the
  app's job queue for it (`jobs.rs`). `tests/recorder.rs` runs the real recorder
  with test tones.
- `vendor/vulkan-sdk/` (git-ignored): glslc, headers and loader from the
  LunarG SDK (`scripts/fetch-vulkan-sdk.sh`) for whisper.cpp's Vulkan
  backend; `.cargo/config.toml` points `VULKAN_SDK` there. A clean build of
  whisper-rs-sys compiles the Vulkan shaders: ~4 minutes.
- `vendor/vosk/` (git-ignored): prebuilt `libvosk.so` from
  `scripts/fetch-vosk.sh`. `build.rs` links it with an rpath to that folder
  (or to `$RECALL_RPATH`; the .deb uses `/opt/recall/lib`).

## Build and test

- `make help` lists everything. `make test` and `make lint` (fmt + clippy
  `-D warnings`) must pass before you finish.
- `make check` says if Rust or `-dev` packages are missing (`make deps`
  installs them; needs the user's sudo).
- Disk space on the user's machine is tight: check `df -h /` before big
  builds; `target/` grows to several GB.

## Driving the running app: control API, never fake input

Never click/type with xdotool/XTest: it lands on the user's real windows.
Use `--control SOCK` / `--ctl SOCK JSON` (README, "Control API"). If
something can't be done through it, add a command to
`crates/core/src/control.rs` and `App::handle_control` in `ui.rs`.

Test instances must not touch the user's setup:

- Run under `xvfb-run -a` and take screenshots *inside* it (`import -window
  root` in the same script; from outside, X auth fails).
- Separate `--config-dir` with `hotkey = ""` (otherwise the test grabs the
  user's Ctrl+Alt+R), `output_dir` in a scratch folder, and
  `XDG_CACHE_HOME` pointing to scratch (or the user's log is overwritten).
- Use `microphone = "file:…wav"` / `others = "file:…wav"` for speech, or
  `"test"` for tones, instead of the real devices. Don't record the real
  mic/output without telling the user; delete such recordings afterwards.
- Test speech: `~/ainews/.venv` has Kokoro TTS (English, Spanish; no
  Russian), models in `~/ainews/models`.
- The global shortcut can only be tested with synthetic keys: do that
  *inside* `xvfb-run` (its own display) with XTest via Python ctypes
  (`libXtst.so.6`), never on the user's display.
- The tray icon registers on the session bus: run test instances under
  `xvfb-run -a dbus-run-session -- …`, or it appears in the user's top bar.
  `tests/tray.rs` uses a private bus (`gio::TestDBus`).
- Start background apps with `setsid … & disown`. Don't `pkill -f
  <pattern>` (matches your own shell); use `pkill -x recall` only when the
  user's own recall is not running.

## Lessons (each cost a debugging session)

- Set the pipeline to PLAYING *before* threads call `pull_sample`: an appsink
  that isn't running yet answers "no more samples", and the thread quits
  (no subtitles/levels for that track, and EOS waits time out).
- Pipelines use the system clock: a pulsesrc clock stops when no sound
  arrives. Sinks of the independent tracks are `async=false`.
- The leaky queue right after each source keeps a blocked source from
  holding back EOS (stop-recording hang).
- Recognition timestamps come from buffer PTS (running time), not from
  counting samples, so skipped audio (when recognition lags) doesn't shift
  later lines.
- Vosk logs to stderr; `vosk::set_log_level(Error)` early. A missing model
  folder is reported as an ERROR line even then; that's expected in
  `--self-test`.

## Whisper lessons

- whisper.cpp's own VAD option (`enable_vad`) only works in `whisper_full()`;
  whisper-rs uses a separate state (`whisper_full_with_state`), where it is
  silently ignored. We run Silero VAD ourselves and pack the speech
  (`speech::pack`); without that Whisper's times drift by seconds on a
  track that is half silence.
- Plain token times drift by seconds too; DTW token times (attention
  alignment, `DtwModelPreset` per model) are what `speech::lines` uses to put
  words into turns, and cuts still prefer sentence ends.
- whisper-rs 0.16 `set_abort_callback_safe` casts its closure to the wrong
  type: every encode then "fails" (error -6). Don't use it.
- Vulkan device 0 here is the Intel iGPU; its driver crashed (C++ exception →
  abort) on turbo. `whisper::usable_gpu` skips integrated Intel graphics;
  transcription runs in a child process so such a crash can't kill the app,
  and is retried with `--no-gpu`.
- Without a punctuated initial prompt (`Language::style_prompt`) Whisper
  sometimes writes lowercase text without punctuation.
- Benchmarks: make a two-voice TTS dialog in a scratch folder (Piper,
  `pip install piper-tts`, has Russian voices `ru_RU-denis/irina-medium`;
  silence on the other track while one talks), encode it like the recorder
  (Opus 32 kb/s), add a `call.json`, and run `recall --transcribe` with
  separate `--config-dir`s per model/`gpu` setting.

## Packaging

- `make deb` (`scripts/build-bundle-deb.sh`) copies everything except the
  libraries matched by `EXCLUDE` into `/opt/recall/lib`; `Depends` comes from
  `dpkg -S` of the excluded ones. A new GStreamer element needs its plugin
  in `PLUGINS` there and an entry in `core::deps::ELEMENTS`.
- Releasing: push a `vX.Y.Z` tag (README, "Release"); the tag's CI run
  publishes the `.deb` on GitHub (`ostretsov/recall`), and installed apps
  then offer it (`core::update`, banner in `app/src/update.rs`).
  `RECALL_UPDATE_URL` replaces the GitHub API URL. Autostart entries
  (`core::autostart`) and update checks happen only for installed packages
  (`/opt/recall`, `/usr/bin/recall`), so dev/test instances never touch the
  user's session; `{"cmd":"check_update"}` checks anyway.
- Icons: `packaging/io.github.ostretsov.recall*.svg` (app; `-recording` is
  the tray icon while recording), installed into hicolor by the deb script.
- `make docker-test` installs the .deb in clean containers and runs
  `recall --self-test` (test tones through the real recorder). Run it after
  packaging changes.
