//! Virtual Cable Mixer — a system tray app that mixes mic + system audio
//! and outputs to VB-Cable for Otter recording.
//!
//! RUST CONCEPT: `mod` and the module system
//! ------------------------------------------
//! `mod audio;` tells Rust "there's a module called audio — look for
//! src/audio/mod.rs". That file then declares its own submodules.
//! This creates a tree: crate -> audio -> {devices, mixer, capture, pipeline}
//!
//! RUST CONCEPT: `fn main() -> Result<()>`
//! ----------------------------------------
//! `main` can return a Result. If it returns `Err(...)`, Rust prints the
//! error and exits with code 1. This replaces the common Python pattern of
//! `if __name__ == "__main__": try: main() except: ...`

mod audio;
mod config;
mod tray;

use anyhow::Result;

fn main() -> Result<()> {
    // Initialize the tracing subscriber for structured logging.
    // This is like Python's `logging.basicConfig()`.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("Starting Virtual Cable Mixer");

    // Create and run the tray app. `run()` blocks until the user quits.
    let app = tray::app::TrayApp::new()?;
    app.run()?;

    Ok(())
}
