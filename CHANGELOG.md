# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Bumped `muda` 0.16 → 0.17, `tray-icon` 0.19 → 0.22, `windows` 0.61 → 0.62.
- Removed unused `rubato` dependency (WASAPI's `AUTOCONVERTPCM` covers all
  resampling needs).
- CI: upgraded to `actions/checkout@v6`, `actions/upload-artifact@v7`,
  `actions/download-artifact@v8`, `softprops/action-gh-release@v3`.

### Known issues

- Tracked in [#10](https://github.com/ytchenak/wasamix/issues/10): migrating to
  `wasapi` 0.23 needs a code-level API update; staying on 0.16 for 0.1.0.

## [0.1.0] - 2026-04-26

### Added

- System-tray app that mixes the selected microphone with system loopback audio and writes the result to VB-Audio Virtual Cable.
- Left-click the tray icon to start/stop mixing; right-click for the mic selector.
- Persisted mic choice in `config.json` next to the executable.
- WASAPI loopback fallback: if the default render device rejects loopback init, the app cycles through other render devices.
- `AUTOCONVERTPCM`-based format normalization so Bluetooth mics (16 kHz native) and stereo f32 loopback streams all flow through the same mono-i16-@-48kHz pipeline.
- Diagnostic binaries `test_capture` and `test_pipeline`.

[Unreleased]: https://github.com/ytchenak/wasamix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ytchenak/wasamix/releases/tag/v0.1.0
