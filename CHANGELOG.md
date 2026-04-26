# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1] - 2026-04-26

### Changed

- Re-recorded the tray demo GIF to reflect the 0.3.0 two-selector menu
  (input microphone + output sound).
- README caption updated to match: the right-click hint now mentions
  picking the mic and what you're listening to, and the running-state
  description matches the color-coded level meter.

## [0.3.0] - 2026-04-26

### Changed

- **Simpler menu: two selectors, not three.** Dropped the
  "Output destination" section. wasamix always writes to VB-Cable; the
  menu now shows only **Input microphone** and **Output sound
  (what you hear)**.
- **Smarter VB-Cable detection.** `find_vbcable` prefers the plain
  `CABLE Input` endpoint over the 16-channel Voicemeeter variant
  (`CABLE In 16ch`). Handles extra standalone cables (`CABLE-B Input`,
  etc.) as tier-2 fallbacks.
- **Output-sound selector no longer lists VB-Cable.** Users can only
  pick speakers / headphones they actually listen through, not
  virtual cables — those are the *destination*, not a source.
- Tooltip shows the resolved destination: `wasamix → CABLE Input — MIXING (-12 dBFS)`.
- Refuses to start when VB-Cable isn't installed; tooltip says
  `wasamix — VB-Audio Cable not installed`.

### Removed

- The "📤 Output destination" menu section and its selector state
  (`selected_output_id`, `output_group`).
- The feedback-loop guard is no longer needed in the UI — the
  source selector can no longer pick a VB-Cable endpoint, so
  source==destination is structurally impossible.

### Kept (for power users)

- `Config.output_device_id` still exists as an unadvertised escape
  hatch: set it in `config.json` to pin a non-default destination
  (e.g. `CABLE-B Input`). README documents this.

## [0.2.0] - 2026-04-26

### Added

- Three-section tray menu — explicit selectors for **input microphone**,
  **system audio source**, and **output destination**. The system-source
  selector includes a `(Windows default)` row that auto-tracks whatever
  Windows is routing now.
- Feedback-loop guard: refuses to use the same device for system source and
  output (both at selection time and at pipeline start), so you can't
  accidentally route the mix back into itself.
- Color-coded tray level meter (grey / dim green / green / amber / red)
  with a live dBFS tooltip. Disable by setting
  `"show_level_meter": false` in `config.json`.
- `peak_i16` helper + `Pipeline::peak_level()` atomic exposure for
  lock-free level polling from the UI thread.

### Changed

- `Config` gained `system_source_device_id` and `output_device_id`. Legacy
  `config.json` files from 0.1.0 keep loading (new fields `#[serde(default)]`
  to `None`, meter defaults to on).
- `Pipeline::start(mic, system_source: Option<&str>, output)` — the old
  two-argument signature (mic + vbcable) is gone.
- Event loop switched from `Poll` to `WaitUntil(166 ms)`; the tray thread
  now actually sleeps between ticks, much lower idle CPU.
- Icon repaints only on bucket transitions to avoid Explorer rate-limit /
  flicker.
- Bumped `muda` 0.16 → 0.17, `tray-icon` 0.19 → 0.22, `windows` 0.61 → 0.62.
- Removed unused `rubato` dependency.
- CI: upgraded to `actions/checkout@v6`, `actions/upload-artifact@v7`,
  `actions/download-artifact@v8`, `softprops/action-gh-release@v3`.

### Known issues

- Tracked in [#10](https://github.com/ytchenak/wasamix/issues/10): migrating
  to `wasapi` 0.23 needs a code-level API update; staying on 0.16 for now.

## [0.1.0] - 2026-04-26

### Added

- System-tray app that mixes the selected microphone with system loopback audio and writes the result to VB-Audio Virtual Cable.
- Left-click the tray icon to start/stop mixing; right-click for the mic selector.
- Persisted mic choice in `config.json` next to the executable.
- WASAPI loopback fallback: if the default render device rejects loopback init, the app cycles through other render devices.
- `AUTOCONVERTPCM`-based format normalization so Bluetooth mics (16 kHz native) and stereo f32 loopback streams all flow through the same mono-i16-@-48kHz pipeline.
- Diagnostic binaries `test_capture` and `test_pipeline`.

[Unreleased]: https://github.com/ytchenak/wasamix/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/ytchenak/wasamix/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/ytchenak/wasamix/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ytchenak/wasamix/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ytchenak/wasamix/releases/tag/v0.1.0
