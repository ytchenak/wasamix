//! System tray application — icon, menu, and user interaction.
//!
//! RUST CONCEPT: Event loops and the ApplicationHandler trait
//! ----------------------------------------------------------
//! GUI applications on all platforms work via an "event loop" — an infinite loop
//! that waits for events (mouse clicks, keyboard, OS messages) and dispatches them
//! to your code. In Rust's `winit` crate, this is modeled as a trait:
//!
//!   trait ApplicationHandler {
//!       fn resumed(&mut self, event_loop: &ActiveEventLoop);
//!       fn window_event(&mut self, ...);
//!       fn about_to_wait(&mut self, event_loop: &ActiveEventLoop);
//!       // ... more methods with default implementations
//!   }
//!
//! You implement this trait on your struct, and the event loop calls your methods
//! when things happen. This is the "Hollywood Principle": don't call us, we'll
//! call you. It's the same pattern as Python's tkinter mainloop() or asyncio's
//! event loop, but expressed through Rust's trait system.
//!
//! RUST CONCEPT: Enums as tagged unions
//! -------------------------------------
//! Rust enums are much more powerful than C/Python enums. Each variant can carry
//! different data. For example, `TrayIconEvent::Click { button, .. }` carries
//! the mouse button, while `TrayIconEvent::Enter { .. }` carries position info.
//! Pattern matching with `match` is the idiomatic way to handle each variant.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::audio::devices::{self, DeviceDirection, DeviceInfo};
use crate::audio::pipeline::Pipeline;
use crate::config::Config;

// --------------------------------------------------------------------------
// Icon dimensions — 64x64 RGBA (4 bytes per pixel)
// --------------------------------------------------------------------------
const ICON_SIZE: u32 = 64;

/// How often the tray thread polls the output-level meter and repaints.
/// ~6 Hz keeps Shell_NotifyIconW happy (Explorer rate-limits faster updates)
/// and gives the human eye enough feedback to see "it's alive".
const TICK_INTERVAL: Duration = Duration::from_millis(166);

/// Coarse output-level buckets. A tray icon is ~16x16 on screen — color is
/// the only thing readable at that size, so we don't animate shape, just hue.
///
/// Thresholds are expressed as i16 peak amplitudes. `Clip` fires at roughly
/// −0.3 dB FS (33100 / 32768).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LevelBucket {
    Idle,   // pipeline not running
    Silent, // running, peak < −40 dB
    Low,    // −40 .. −20 dB
    Mid,    // −20 .. −3 dB
    Clip,   // ≥ −3 dB
}

impl LevelBucket {
    /// Map a peak amplitude (0..=32768) to a bucket. Callers pass `None`
    /// when the pipeline is stopped.
    fn from_peak(peak: Option<u16>) -> Self {
        let Some(p) = peak else {
            return LevelBucket::Idle;
        };
        // Thresholds chosen for useful visual feedback:
        //   0.01 * 32768 ≈ 328   (−40 dB)
        //   0.1  * 32768 ≈ 3277  (−20 dB)
        //   0.7  * 32768 ≈ 22938 (−3 dB)
        match p {
            0..=327 => LevelBucket::Silent,
            328..=3276 => LevelBucket::Low,
            3277..=22937 => LevelBucket::Mid,
            _ => LevelBucket::Clip,
        }
    }

    fn color(self) -> (u8, u8, u8) {
        match self {
            LevelBucket::Idle => (128, 128, 128), // grey
            LevelBucket::Silent => (30, 90, 30),  // dim green
            LevelBucket::Low => (0, 200, 0),      // green
            LevelBucket::Mid => (230, 190, 0),    // amber
            LevelBucket::Clip => (220, 40, 40),   // red
        }
    }
}

/// Human-readable dB estimate for the tooltip. Returns `-inf` for silence
/// rather than doing ugly log(0) arithmetic.
fn peak_to_dbfs(peak: u16) -> f32 {
    if peak == 0 {
        return f32::NEG_INFINITY;
    }
    20.0 * (peak as f32 / 32768.0).log10()
}

/// Append a disabled header row. On most Windows themes this renders in
/// grey, reading as a section label rather than a clickable command.
fn append_header(menu: &Menu, text: &str) -> Result<()> {
    let item = MenuItem::new(text, false, None);
    menu.append(&item).context("Failed to append menu header")
}

/// Trim the trailing vendor tag from VB-Audio device names for tooltip
/// display. `CABLE Input (VB-Audio Virtual Cable)` → `CABLE Input`.
fn shorten_cable_name(name: &str) -> String {
    match name.find(" (") {
        Some(i) => name[..i].to_string(),
        None => name.to_string(),
    }
}

/// A radio-group selector rendered as a sequence of `CheckMenuItem`s plus
/// menu-id ↔ value maps. `None` as the value means "(auto) — whatever the
/// default resolves to right now" (only used by the system-source group).
struct MenuGroup {
    items: Vec<CheckMenuItem>,
    // (MenuId, Some(device_id)) or (MenuId, None) for the "Windows default" row
    id_map: Vec<(MenuId, Option<String>)>,
}

impl MenuGroup {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            id_map: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.items.clear();
        self.id_map.clear();
    }

    /// If `menu_id` belongs to this group, return the associated device-id
    /// selection. `Ok(Some(Some(id)))` is a concrete device; `Ok(Some(None))`
    /// is the "Windows default" row; `Ok(None)` means "not ours".
    fn resolve(&self, menu_id: &MenuId) -> Option<Option<String>> {
        self.id_map
            .iter()
            .find(|(m, _)| m == menu_id)
            .map(|(_, v)| v.clone())
    }

    fn set_checked_by_value(&self, target: Option<&str>) {
        for ((_, value), item) in self.id_map.iter().zip(self.items.iter()) {
            let checked = match (value, target) {
                (None, None) => true,
                (Some(v), Some(t)) => v == t,
                _ => false,
            };
            item.set_checked(checked);
        }
    }
}

/// System tray application — owns the tray icon, context menu, audio pipeline,
/// and configuration state.
pub struct TrayApp {
    /// The tray icon handle — `Option` because we create it after the event
    /// loop starts (in `resumed()`).
    tray_icon: Option<TrayIcon>,

    /// The running audio pipeline — `None` when idle, `Some(pipeline)` when mixing.
    pipeline: Option<Pipeline>,

    /// Persisted user config.
    config: Config,

    /// All available input (microphone) devices.
    input_devices: Vec<DeviceInfo>,

    /// "Real" render devices (speakers, headphones) the user can listen
    /// through. VB-Cable endpoints are filtered out up-stream.
    render_devices: Vec<DeviceInfo>,

    /// ID of the Windows-default render device at startup — used to tag the
    /// corresponding row in the system-source selector.
    default_render_id: Option<String>,

    /// Resolved VB-Cable destination: concrete `Some(id)` of the preferred
    /// CABLE Input, or `None` if VB-Cable isn't installed. Set once at
    /// startup; not user-configurable from the menu. Power users can pin a
    /// specific ID via `output_device_id` in `config.json`.
    vbcable_id: Option<String>,

    /// Friendly name of the resolved destination, for the tooltip.
    vbcable_name: Option<String>,

    /// Currently selected microphone device ID.
    selected_mic_id: Option<String>,

    /// Currently selected system-audio source: `None` = "Windows default",
    /// `Some(id)` = pinned render device.
    selected_system_source_id: Option<String>,

    /// The context menu shown on right-click.
    menu: Option<Menu>,

    /// Radio-group state for each selector.
    mic_group: MenuGroup,
    system_source_group: MenuGroup,

    /// The "Quit" menu item — stored so we can identify its ID in event handling.
    quit_item_id: Option<MenuId>,

    /// Last bucket we pushed to the tray icon. Used to skip redundant
    /// `set_icon`/`set_tooltip` calls (Explorer hates frequent updates).
    current_bucket: LevelBucket,

    /// Next time we should poll the peak level atomic and potentially update
    /// the icon. Using `Instant` (monotonic clock) avoids wall-clock jumps.
    next_tick: Instant,
}

impl TrayApp {
    /// Create a new TrayApp — loads config, enumerates devices, selects default mic.
    ///
    /// RUST CONCEPT: Constructor pattern
    /// ----------------------------------
    /// Rust doesn't have constructors. By convention, `Type::new()` is the
    /// "constructor" — it's just a regular associated function that returns `Self`.
    pub fn new() -> Result<Self> {
        // Load saved config (or defaults if no config file exists)
        let config = Config::load();
        info!(
            "Config loaded: mic={:?} system_source={:?} output={:?}",
            config.mic_device_id, config.system_source_device_id, config.output_device_id
        );

        // Enumerate all audio devices on the system
        let all_devices =
            devices::enumerate_devices().context("Failed to enumerate audio devices")?;

        let input_devices = devices::filter_input_devices(&all_devices);
        let render_devices = devices::filter_render_devices(&all_devices);
        let default_render_id = devices::get_default_render_device_id_opt();

        // Resolve the mix destination. Normally auto-detected (prefer
        // plain "CABLE Input" over "CABLE In 16ch"); power users can pin
        // a specific endpoint via Config::output_device_id.
        let pinned_output = config.output_device_id.as_deref().and_then(|pinned| {
            all_devices
                .iter()
                .find(|d| d.direction == DeviceDirection::Render && d.id == pinned)
        });
        let vbcable = pinned_output.or_else(|| devices::find_vbcable(&all_devices));
        let vbcable_id = vbcable.map(|d| d.id.clone());
        let vbcable_name = vbcable.map(|d| d.name.clone());

        info!("Found {} input device(s)", input_devices.len());
        for d in &input_devices {
            info!("  - {} ({})", d.name, d.id);
        }
        info!("Found {} render device(s)", render_devices.len());
        for d in &render_devices {
            info!("  - {} ({})", d.name, d.id);
        }
        match &vbcable_name {
            Some(n) => info!("Mix destination: {}", n),
            None => warn!("VB-Cable not found — install VB-Audio Virtual Cable to use wasamix"),
        }

        // Mic: saved config > first available > none.
        let selected_mic_id = config
            .mic_device_id
            .clone()
            .filter(|saved| input_devices.iter().any(|d| &d.id == saved))
            .or_else(|| input_devices.first().map(|d| d.id.clone()));

        // System source: saved config if still present on the system, else
        // None (which means "Windows default, with fallback").
        let selected_system_source_id = config
            .system_source_device_id
            .clone()
            .filter(|saved| render_devices.iter().any(|d| &d.id == saved));

        info!(
            "Initial selection: mic={:?} system_source={:?}",
            selected_mic_id, selected_system_source_id
        );

        Ok(TrayApp {
            tray_icon: None,
            pipeline: None,
            config,
            input_devices,
            render_devices,
            default_render_id,
            vbcable_id,
            vbcable_name,
            selected_mic_id,
            selected_system_source_id,
            menu: None,
            mic_group: MenuGroup::new(),
            system_source_group: MenuGroup::new(),
            quit_item_id: None,
            current_bucket: LevelBucket::Idle,
            next_tick: Instant::now(),
        })
    }

    /// Build (or rebuild) the right-click context menu.
    ///
    /// Two selector sections — input mic, output sound (what you listen
    /// through; drives loopback capture) — plus Quit. The mix destination
    /// (VB-Cable) is resolved automatically and not exposed in the menu.
    /// All selectors are disabled while mixing; stop, switch, start.
    fn build_menu(&mut self) -> Result<Menu> {
        let menu = Menu::new();
        let is_mixing = self.pipeline.is_some();

        self.mic_group.clear();
        self.system_source_group.clear();

        // --- Section 1: input microphone -----------------------------------
        append_header(&menu, "\u{1F399}  Input microphone:")?;
        for device in &self.input_devices {
            let is_selected = self.selected_mic_id.as_deref() == Some(&device.id);
            let item = CheckMenuItem::new(&device.name, !is_mixing, is_selected, None);
            self.mic_group
                .id_map
                .push((item.id().clone(), Some(device.id.clone())));
            self.mic_group.items.push(item.clone());
            menu.append(&item).context("Failed to append mic item")?;
        }
        if self.input_devices.is_empty() {
            menu.append(&MenuItem::new("(no microphones found)", false, None))
                .context("Failed to append mic placeholder")?;
        }

        // --- Section 2: output sound (loopback source) --------------------
        // Named "Output sound (what you hear)" because that's what users
        // actually think about — not "system audio source". The selection
        // here is the render device we loopback-capture to feed the mix.
        menu.append(&PredefinedMenuItem::separator())
            .context("Failed to append separator")?;
        append_header(&menu, "\u{1F50A} Output sound (what you hear):")?;

        let default_row_label = match self.default_render_id.as_ref() {
            Some(id) => match self.render_devices.iter().find(|d| &d.id == id) {
                Some(d) => format!("(Windows default — {})", d.name),
                None => "(Windows default)".to_string(),
            },
            None => "(Windows default)".to_string(),
        };
        let default_row = CheckMenuItem::new(
            &default_row_label,
            !is_mixing,
            self.selected_system_source_id.is_none(),
            None,
        );
        self.system_source_group
            .id_map
            .push((default_row.id().clone(), None));
        self.system_source_group.items.push(default_row.clone());
        menu.append(&default_row)
            .context("Failed to append default-render row")?;

        for device in &self.render_devices {
            let is_selected = self.selected_system_source_id.as_deref() == Some(&device.id);
            let item = CheckMenuItem::new(&device.name, !is_mixing, is_selected, None);
            self.system_source_group
                .id_map
                .push((item.id().clone(), Some(device.id.clone())));
            self.system_source_group.items.push(item.clone());
            menu.append(&item)
                .context("Failed to append output-sound item")?;
        }
        if self.render_devices.is_empty() {
            menu.append(&MenuItem::new("(no output devices found)", false, None))
                .context("Failed to append output-sound placeholder")?;
        }

        // --- Quit ----------------------------------------------------------
        menu.append(&PredefinedMenuItem::separator())
            .context("Failed to append separator before Quit")?;
        let quit_item = MenuItem::new("Quit", true, None);
        self.quit_item_id = Some(quit_item.id().clone());
        menu.append(&quit_item)
            .context("Failed to append quit item")?;

        Ok(menu)
    }

    /// Create a 64x64 RGBA filled-circle icon in the given RGB color.
    fn create_icon_rgb(r: u8, g: u8, b: u8) -> Icon {
        // Allocate RGBA buffer: 64 * 64 * 4 bytes
        let mut rgba = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

        let center = ICON_SIZE as f64 / 2.0;
        let radius = center - 2.0; // slight margin so the circle isn't clipped

        for y in 0..ICON_SIZE {
            for x in 0..ICON_SIZE {
                let dx = x as f64 - center;
                let dy = y as f64 - center;
                let distance = (dx * dx + dy * dy).sqrt();

                let offset = ((y * ICON_SIZE + x) * 4) as usize;

                if distance <= radius {
                    // Inside the circle — fill with our color
                    rgba[offset] = r;
                    rgba[offset + 1] = g;
                    rgba[offset + 2] = b;
                    rgba[offset + 3] = 255; // fully opaque
                } else {
                    // Outside the circle — transparent
                    rgba[offset] = 0;
                    rgba[offset + 1] = 0;
                    rgba[offset + 2] = 0;
                    rgba[offset + 3] = 0;
                }
            }
        }

        // Icon::from_rgba returns Result because it validates dimensions.
        // We know our data is correct, so `.expect()` is safe here.
        Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE)
            .expect("Icon RGBA data should be valid (64x64x4 bytes)")
    }

    fn icon_for(bucket: LevelBucket) -> Icon {
        let (r, g, b) = bucket.color();
        Self::create_icon_rgb(r, g, b)
    }

    /// Toggle mixing on/off — called on left-click.
    ///
    /// If currently mixing: stop the pipeline.
    /// If currently idle: start the pipeline (if mic + VB-Cable are available).
    fn toggle_mixing(&mut self) {
        if self.pipeline.is_some() {
            self.stop_mixing();
        } else {
            self.start_mixing();
        }
    }

    /// Start the audio pipeline — mic + system loopback → output device.
    ///
    fn start_mixing(&mut self) {
        let Some(mic_id) = self.selected_mic_id.clone() else {
            warn!("Cannot start mixing: no microphone selected");
            return;
        };

        let Some(output_id) = self.vbcable_id.clone() else {
            error!(
                "Cannot start mixing: VB-Audio Virtual Cable not found. Install it from \
                 https://vb-audio.com/Cable/ — wasamix writes the mix there so your recording \
                 app can pick it up as a microphone."
            );
            return;
        };

        info!(
            "Starting pipeline: mic={} system_source={:?} output={}",
            mic_id, self.selected_system_source_id, output_id
        );

        match Pipeline::start(
            &mic_id,
            self.selected_system_source_id.as_deref(),
            &output_id,
        ) {
            Ok(pipeline) => {
                self.pipeline = Some(pipeline);
                // Force the next update_icon() tick to repaint: we're
                // transitioning out of Idle, and the new bucket depends on
                // live peak data we haven't read yet.
                self.current_bucket = LevelBucket::Idle;
                self.update_icon();
                self.rebuild_menu();
                info!("Mixing started");
            }
            Err(e) => {
                error!("Failed to start pipeline: {:?}", e);
            }
        }
    }

    /// Stop the audio pipeline.
    ///
    /// RUST CONCEPT: `Drop` and `Option::take()`
    /// `self.pipeline.take()` extracts the Pipeline from the Option (leaving None).
    /// When the extracted Pipeline goes out of scope at the end of this block,
    /// its `Drop` implementation runs, which calls `stop()` on all threads.
    fn stop_mixing(&mut self) {
        if let Some(mut pipeline) = self.pipeline.take() {
            pipeline.stop();
            info!("Mixing stopped");
        }
        self.update_icon();
        self.rebuild_menu();
    }

    /// Poll the current peak level and update the tray icon + tooltip.
    ///
    /// Called from `about_to_wait` on the UI timer. We only hit
    /// `set_icon`/`set_tooltip` when the bucket actually changes, so the
    /// shell doesn't see more than ~1 update per noticeable level shift.
    fn update_icon(&mut self) {
        // Decide which bucket applies right now.
        let bucket = if self.pipeline.is_some() {
            if self.config.show_level_meter {
                let peak = self.pipeline.as_ref().map(|p| p.peak_level());
                LevelBucket::from_peak(peak)
            } else {
                // Meter disabled: behave like pre-meter wasamix — solid green
                // whenever mixing is running.
                LevelBucket::Low
            }
        } else {
            LevelBucket::Idle
        };

        let Some(tray) = &self.tray_icon else { return };

        // Only re-push the icon when the bucket changes. Explorer will drop
        // / coalesce updates otherwise, causing flicker on some themes.
        if bucket != self.current_bucket {
            let icon = Self::icon_for(bucket);
            if let Err(e) = tray.set_icon(Some(icon)) {
                error!("Failed to update tray icon: {:?}", e);
            }
            self.current_bucket = bucket;
        }

        // Tooltip is cheap to recompute and users see the dB value update
        // as they hover — so we refresh it every tick while mixing.
        let dest_hint = match self.vbcable_name.as_deref() {
            Some(name) => format!(" → {}", shorten_cable_name(name)),
            None => " · VB-Cable not installed".to_string(),
        };

        let tooltip = match (bucket, self.pipeline.as_ref()) {
            (LevelBucket::Idle, _) if self.vbcable_id.is_none() => {
                "wasamix — VB-Audio Cable not installed".to_string()
            }
            (LevelBucket::Idle, _) => {
                format!("wasamix — idle (click to start{})", dest_hint)
            }
            (_, Some(pipeline)) if self.config.show_level_meter => {
                let db = peak_to_dbfs(pipeline.peak_level());
                if db.is_finite() {
                    format!("wasamix{} — MIXING ({:.0} dBFS)", dest_hint, db)
                } else {
                    format!("wasamix{} — MIXING (silent)", dest_hint)
                }
            }
            _ => format!("wasamix{} — MIXING", dest_hint),
        };
        let _ = tray.set_tooltip(Some(&tooltip));
    }

    /// Rebuild the menu and update the tray icon's menu reference.
    fn rebuild_menu(&mut self) {
        match self.build_menu() {
            Ok(menu) => {
                if let Some(tray) = &self.tray_icon {
                    tray.set_menu(Some(Box::new(menu.clone())));
                }
                self.menu = Some(menu);
            }
            Err(e) => {
                error!("Failed to rebuild menu: {:?}", e);
            }
        }
    }

    /// Selecting any option does NOT auto-start mixing — the user must
    /// left-click the icon to begin. This keeps intent explicit.
    fn select_mic(&mut self, device_id: &str) {
        info!("Mic selected: {}", device_id);
        self.selected_mic_id = Some(device_id.to_string());
        self.config.mic_device_id = Some(device_id.to_string());
        self.persist_config();
        self.mic_group.set_checked_by_value(Some(device_id));
    }

    fn select_system_source(&mut self, device_id: Option<&str>) {
        info!("System source selected: {:?}", device_id);
        self.selected_system_source_id = device_id.map(|s| s.to_string());
        self.config.system_source_device_id = self.selected_system_source_id.clone();
        self.persist_config();
        self.system_source_group.set_checked_by_value(device_id);
    }

    fn persist_config(&self) {
        if let Err(e) = self.config.save() {
            error!("Failed to save config: {:?}", e);
        }
    }

    /// Route menu events to the right selector group (or Quit).
    fn handle_menu_event(&mut self, event: MenuEvent) {
        if let Some(quit_id) = &self.quit_item_id
            && event.id == *quit_id
        {
            info!("Quit selected — shutting down");
            self.stop_mixing();
            self.tray_icon = None;
            return;
        }

        if let Some(Some(dev_id)) = self.mic_group.resolve(&event.id) {
            self.select_mic(&dev_id);
            return;
        }

        if let Some(value) = self.system_source_group.resolve(&event.id) {
            self.select_system_source(value.as_deref());
        }
    }

    /// Handle a tray icon event (left-click toggles mixing).
    ///
    /// RUST CONCEPT: Exhaustive pattern matching
    /// The `match` expression on an enum must cover all variants (or use `_`
    /// as a wildcard). This ensures we don't forget to handle a case —
    /// the compiler checks this at compile time.
    fn handle_tray_event(&mut self, event: TrayIconEvent) {
        // Left-click release = toggle mixing on/off.
        // Right-clicks (menu handled by the OS), double-clicks, hover/enter/leave
        // are all intentionally ignored.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            self.toggle_mixing();
        }
    }

    /// Run the tray application — builds the menu, creates the tray icon,
    /// and enters the winit event loop. This function blocks until the user quits.
    ///
    /// RUST CONCEPT: `self` (by value) — ownership transfer
    /// This method takes `self` by value (not `&self` or `&mut self`), meaning
    /// it *consumes* the TrayApp. The caller can't use it afterward. This makes
    /// sense because `run()` enters an infinite event loop — there's nothing to
    /// do with the TrayApp after it returns.
    pub fn run(mut self) -> Result<()> {
        // Build the event loop. On Windows, winit creates a hidden window
        // internally to receive OS messages (WM_COMMAND, etc.).
        //
        // RUST CONCEPT: `EventLoop::new()` returns `Result`
        // Creating an event loop can fail (e.g., if one already exists).
        // We use `?` to propagate the error.
        let event_loop = EventLoop::new().context("Failed to create event loop")?;

        // Initial control flow — overridden each tick by about_to_wait with
        // a concrete WaitUntil so we wake up on a 6 Hz cadence for the level
        // meter and event polling. This is gentler on the CPU than Poll and
        // still responsive enough for tray UX.
        self.next_tick = Instant::now() + TICK_INTERVAL;
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));

        // Build the context menu
        let menu = self.build_menu().context("Failed to build context menu")?;
        self.menu = Some(menu.clone());

        // Create the tray icon. The first-tick update_icon() will replace
        // this tooltip with the destination-aware one a moment later.
        let initial_tooltip = match self.vbcable_name.as_deref() {
            Some(name) => format!(
                "wasamix — idle (click to start → {})",
                shorten_cable_name(name)
            ),
            None => "wasamix — VB-Audio Cable not installed".to_string(),
        };
        let icon = Self::icon_for(LevelBucket::Idle);
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(&initial_tooltip)
            .with_icon(icon)
            .with_menu_on_left_click(false) // we handle left-click ourselves
            .build()
            .context("Failed to create tray icon")?;

        self.tray_icon = Some(tray_icon);

        // Enter the event loop — this blocks until exit.
        // `run_app` calls our `ApplicationHandler` methods.
        event_loop.run_app(&mut self).context("Event loop error")?;

        info!("Tray app exited cleanly");
        Ok(())
    }
}

/// RUST CONCEPT: Implementing a trait
/// ------------------------------------
/// `impl ApplicationHandler for TrayApp` means TrayApp satisfies the
/// `ApplicationHandler` contract. The event loop will call these methods:
///
/// - `resumed()`: called when the app starts (and after suspend/resume on mobile)
/// - `window_event()`: called when a window event occurs (we have no windows, so this is a no-op)
/// - `about_to_wait()`: called on each loop iteration — this is where we poll for tray/menu events
///
/// The trait has default implementations for methods we don't need to override.
impl ApplicationHandler for TrayApp {
    /// Called when the application is resumed. On desktop platforms, this fires
    /// once at startup right after the event loop starts.
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        // Nothing to do here — our tray icon is already created in `run()`.
        // On mobile platforms, this would be where you recreate render surfaces.
    }

    /// Called for window events. We don't create any windows (just a tray icon),
    /// so this is a no-op. But the trait requires us to implement it.
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
        // No windows in this app — the tray icon lives in the system tray,
        // not in a winit window.
    }

    /// Called on each event loop iteration, right before the loop might block
    /// waiting for new events. This is our "tick" — we poll for tray and menu
    /// events here.
    ///
    /// RUST CONCEPT: Channel-based event handling
    /// `TrayIconEvent::receiver()` returns a reference to a global channel receiver.
    /// `try_recv()` is non-blocking: it returns `Ok(event)` if an event is waiting,
    /// or `Err(TryRecvError::Empty)` if not. This is similar to Python's `queue.get_nowait()`.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Poll for tray icon events (clicks, hover, etc.)
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            self.handle_tray_event(event);
        }

        // Poll for menu events (mic selection, quit)
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.handle_menu_event(event);
        }

        // Level-meter tick: if the deadline has passed, refresh the icon +
        // tooltip from the pipeline's peak level, then schedule the next
        // wake-up. Using WaitUntil (not Poll) means the thread actually
        // sleeps between ticks — negligible CPU when idle.
        let now = Instant::now();
        if now >= self.next_tick {
            self.update_icon();
            self.next_tick = now + TICK_INTERVAL;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));

        // If the tray icon was dropped (user quit), exit the event loop.
        if self.tray_icon.is_none() {
            event_loop.exit();
        }
    }
}
