# Contributing to wasamix

Thanks for your interest. This document covers what you need to know to send a useful PR.

## Before you start

- **Bug reports & small fixes** — just open an issue or PR.
- **Features / refactors** — open an issue first to check the direction. wasamix is deliberately small (one tray icon, one job). Scope-creep PRs are likely to be declined.
- **Platform support** — wasamix is Windows-only and will stay that way. WASAPI is load-bearing. Ports to CoreAudio / PipeWire belong in a separate project.

## Dev setup

```bash
git clone https://github.com/ytchenak/wasamix.git
cd wasamix
cargo build
cargo test
cargo run
```

If the linker fails with something about `link.exe`, read the comment at the top of [`.cargo/config.toml`](./.cargo/config.toml). Git for Windows ships its own `link.exe` (a Unix symlink util) that shadows MSVC's. The config pins an absolute path; update it to match your installed MSVC toolchain if needed.

## Code expectations

Before you push:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs these on every PR and will block on failures.

### Comment style

The existing code has extensive teaching-oriented comments aimed at developers coming from Python. If you're editing that code, keep the tone consistent. **For new code, comment sparingly** — only when the *why* isn't obvious from the names. See [CLAUDE.md](./CLAUDE.md) for the rationale.

### Architecture

Read [CLAUDE.md](./CLAUDE.md) first — it covers the three-thread pipeline, the mono-i16-@-48kHz canonical format, the shutdown protocol, and the WASAPI quirks (per-thread COM init, Bluetooth loopback fallback, `AUTOCONVERTPCM`).

### Tests

Unit tests live in `#[cfg(test)] mod tests` at the bottom of each module. Integration-style binaries (that hit real audio devices) go under `src/bin/`. The `test_capture` and `test_pipeline` binaries are not run in CI — they need hardware.

## Pull request checklist

- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo test` passing
- [ ] If you changed behavior, updated `README.md` and/or `CLAUDE.md`
- [ ] Added an entry to `CHANGELOG.md` under `## [Unreleased]` for user-visible changes
- [ ] Commit messages are readable (one logical change per commit is nice; not required)

## Licensing of contributions

By submitting a PR you agree that your contribution is dual-licensed under **MIT OR Apache-2.0** — same as the project. No CLA to sign; this is the standard Rust-ecosystem arrangement.

## Releasing (maintainers)

1. Bump version in `Cargo.toml`.
2. Move `## [Unreleased]` entries in `CHANGELOG.md` under a new `## [x.y.z] - YYYY-MM-DD` heading.
3. Commit, tag `vX.Y.Z`, push the tag. The release workflow builds the Windows binary and publishes a GitHub release.
