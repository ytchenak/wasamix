# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`wasamix` — a Windows-only system tray app that mixes microphone input + system loopback audio and writes the result to VB-Audio Virtual Cable for downstream recording (e.g., Otter). Built on WASAPI, `tray-icon`/`muda`/`winit` for the tray UI, `rubato` for resampling, and `anyhow`/`tracing` for errors/logging.

Source comments are intentionally teaching-oriented (Rust concepts explained for a Python-background reader). Preserve that tone when editing existing comments; for new code, follow the guidance in the top-level system prompt (avoid narrative comments).

## Commands

```bash
cargo build                       # debug build
cargo build --release             # release build
cargo run                         # runs default binary (wasamix) — the tray app
cargo run --bin test_pipeline     # 5-second end-to-end mic+loopback->VB-Cable smoke test
cargo run --bin test_capture      # WASAPI loopback diagnostic: probes every render device
cargo test                        # runs all unit tests (#[cfg(test)] modules in src/)
cargo test test_mix_clamp_positive        # run a single test by name
cargo test audio::mixer                   # run all tests in a module path
```

There is no lint/format config checked in — use `cargo fmt` / `cargo clippy` if needed.

## Platform constraints

- **Windows only.** WASAPI + `windows` crate + `tray-icon` assume Windows; there is no cross-platform story.
- **`.cargo/config.toml` pins the MSVC linker to an absolute path** because Git for Windows ships a `usr/bin/link.exe` (the Unix symlink utility) that shadows MSVC's `link.exe` on `PATH`. If a fresh toolchain install changes the MSVC version, that path needs updating — don't remove the override.
- `edition = "2024"` — requires a recent Rust toolchain.

## Runtime prerequisites

- **VB-Audio Virtual Cable** must be installed. The app locates it by friendly-name match on `"cable input"` (render direction). Without it, the tray menu shows the icon but mixing is disabled.
- Config is persisted as `config.json` next to the running `.exe` (not in `%APPDATA%`). Only the selected `mic_device_id` is stored.

## Architecture

The app is three layers, bottom to top:

**1. `audio::devices`** — WASAPI device enumeration. Thin wrappers that return plain `DeviceInfo { id, name, direction }`. The VB-Cable render endpoint is (confusingly) named "CABLE Input" — that is intentional from VB-Audio's perspective and is the string we match on.

**2. `audio::{mixer, capture, pipeline}`** — the real-time audio core. Internal canonical format is **mono i16 @ 48 kHz**; everything converts to that on the way in and we write that format on the way out. Three threads run concurrently when mixing:

```
[mic capture thread]   --i16 mono--> [mic RingBuffer]    \
                                                           -> [render thread] -> VB-Cable
[loopback thread]      --i16 mono--> [loopback RingBuffer]/
```

- Each ring buffer is an `Arc<Mutex<RingBuffer>>`. On overflow the oldest bytes are dropped; on underflow `read()` pads with silence. Buffer capacity is 2 seconds.
- The render thread pulls `bytes_needed` from both buffers through a closure (`Arc<Mutex<dyn FnMut(usize) -> Vec<u8> + Send>>`) and calls `mix_samples` (sample-wise add, clamp to i16 range).
- Shutdown is coordinated by a single `Arc<AtomicBool>` stop flag polled in each loop. `Pipeline::stop()` sets it and joins all three threads; `impl Drop for Pipeline` guarantees this runs even on panic/early return.
- **Every thread that touches WASAPI must call `wasapi::initialize_mta()` on entry** — COM is per-thread on Windows.
- **Mic path** requests a specific format (mono i16 48 kHz) with `autoconvert = true` so WASAPI resamples devices like 16 kHz Bluetooth headsets for us.
- **Loopback path** accepts the default render device's mix format (usually stereo f32 48 kHz) and converts in code (`convert_f32_to_mono_i16` / `convert_i16_to_mono`). It has a fallback loop: if the default render device fails `Initialize` for loopback (common with Bluetooth devices in certain states), it tries other render devices, skipping anything whose name contains `"cable"`.
- **Render path** writes mono i16 48 kHz with `autoconvert = true` so we don't care about VB-Cable's actual native format.

**3. `tray::app::TrayApp`** — the UI layer. Implements `winit::application::ApplicationHandler`; `about_to_wait` polls `TrayIconEvent::receiver()` and `MenuEvent::receiver()` each tick (ControlFlow::Poll).

- Left-click toggles the pipeline (`start_mixing` / `stop_mixing`). The tray icon is a programmatically-generated 64x64 circle — grey when idle, green when mixing.
- Right-click shows a radio-style menu of mic `CheckMenuItem`s plus Quit. Selecting a mic **only updates config**; it does not start mixing (deliberate UX — left-click is the sole start trigger).
- Mic menu items are disabled while mixing to prevent mid-stream device switches.
- `MenuId`/device-id mapping is kept in a `Vec<(MenuId, String)>` (small n, linear scan is fine).
- Quit is signalled by dropping `self.tray_icon`; `about_to_wait` notices the `None` and calls `event_loop.exit()`.

## Conventions worth knowing

- Errors use `anyhow::Result` throughout; convert third-party errors with `.map_err(|e| anyhow::anyhow!(...))` or `.context(...)`.
- Logging is `tracing` (`info!`/`warn!`/`error!`/`debug!`). `main.rs` sets up a default subscriber at `INFO`.
- New submodules go under `src/audio/` or `src/tray/` and must be declared in the module's `mod.rs`. `src/lib.rs` re-exports `audio`, `config`, `tray` as `pub` so the `src/bin/*.rs` helpers can use them.
- New diagnostic / integration binaries go in `src/bin/` — Cargo auto-discovers them; `default-run = "wasamix"` in `Cargo.toml` keeps `cargo run` pointing at the tray app.
