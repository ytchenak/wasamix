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

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::audio::devices::{self, DeviceInfo};
use crate::audio::pipeline::Pipeline;
use crate::config::Config;

// --------------------------------------------------------------------------
// Icon dimensions — 64x64 RGBA (4 bytes per pixel)
// --------------------------------------------------------------------------
const ICON_SIZE: u32 = 64;

/// System tray application — owns the tray icon, context menu, audio pipeline,
/// and configuration state.
///
/// RUST CONCEPT: Struct composition (vs. inheritance)
/// ---------------------------------------------------
/// Rust doesn't have class inheritance. Instead, you compose structs — the
/// `TrayApp` *contains* a `Pipeline`, a `Config`, a `TrayIcon`, etc. Behavior
/// is defined through trait implementations (like `ApplicationHandler`) rather
/// than by inheriting from a base class.
pub struct TrayApp {
    /// The tray icon handle — `Option` because we create it after the event
    /// loop starts (in `resumed()`).
    tray_icon: Option<TrayIcon>,

    /// The running audio pipeline — `None` when idle, `Some(pipeline)` when mixing.
    ///
    /// RUST CONCEPT: `Option<T>` for state machines
    /// Option naturally models "present or absent" states. When `pipeline` is
    /// `Some`, mixing is active. When `None`, we're idle. The compiler ensures
    /// we handle both cases.
    pipeline: Option<Pipeline>,

    /// Persisted user config (selected mic device ID).
    config: Config,

    /// All available input (microphone) devices.
    input_devices: Vec<DeviceInfo>,

    /// Device ID of the VB-Cable render endpoint (where we write mixed audio).
    vbcable_id: Option<String>,

    /// Currently selected microphone device ID.
    selected_mic_id: Option<String>,

    /// The context menu shown on right-click.
    menu: Option<Menu>,

    /// Maps each menu item's MenuId to its audio device ID string.
    /// This lets us look up which device the user selected when a menu event fires.
    ///
    /// RUST CONCEPT: Vec<(A, B)> as a simple association list
    /// For small collections, a Vec of tuples is often simpler and faster than
    /// a HashMap. We iterate linearly to find a match — O(n) but n is tiny
    /// (number of microphones on the system, typically 1-5).
    mic_menu_ids: Vec<(MenuId, String)>,

    /// Stores the CheckMenuItem handles so we can update their checked/enabled state.
    mic_menu_items: Vec<CheckMenuItem>,

    /// The "Quit" menu item — stored so we can identify its ID in event handling.
    quit_item_id: Option<MenuId>,
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
        info!("Config loaded: mic_device_id={:?}", config.mic_device_id);

        // Enumerate all audio devices on the system
        let all_devices =
            devices::enumerate_devices().context("Failed to enumerate audio devices")?;

        // Find the VB-Cable render endpoint
        let vbcable_id = devices::find_vbcable(&all_devices).map(|d| d.id.clone());
        if vbcable_id.is_none() {
            warn!("VB-Cable not found — mixing will not be available");
        }

        // Filter to just microphone (capture) devices, excluding VB-Cable endpoints
        let input_devices = devices::filter_input_devices(&all_devices);
        info!("Found {} input device(s)", input_devices.len());
        for d in &input_devices {
            info!("  - {} ({})", d.name, d.id);
        }

        // Determine which mic to select: saved config > first available > none
        //
        // RUST CONCEPT: `Option` chaining with `.or_else()`
        // `.or_else(|| ...)` provides a fallback: if the first Option is None,
        // evaluate the closure to get another Option. This chains gracefully
        // without nested if/else.
        let selected_mic_id = config
            .mic_device_id
            .clone()
            .filter(|saved_id| input_devices.iter().any(|d| &d.id == saved_id))
            .or_else(|| input_devices.first().map(|d| d.id.clone()));

        info!("Selected mic: {:?}", selected_mic_id);

        Ok(TrayApp {
            tray_icon: None,
            pipeline: None,
            config,
            input_devices,
            vbcable_id,
            selected_mic_id,
            menu: None,
            mic_menu_ids: Vec::new(),
            mic_menu_items: Vec::new(),
            quit_item_id: None,
        })
    }

    /// Build (or rebuild) the right-click context menu.
    ///
    /// The menu has:
    ///   - A CheckMenuItem for each microphone (radio-button style: exactly one checked)
    ///   - A separator
    ///   - A "Quit" item
    ///
    /// RUST CONCEPT: Mutable borrowing (`&mut self`)
    /// This method takes `&mut self` because it modifies `self.mic_menu_ids`,
    /// `self.mic_menu_items`, and `self.quit_item_id`. Rust's borrow checker
    /// ensures no one else is reading these fields while we mutate them.
    fn build_menu(&mut self) -> Result<Menu> {
        let menu = Menu::new();
        let is_mixing = self.pipeline.is_some();

        // Clear old mappings
        self.mic_menu_ids.clear();
        self.mic_menu_items.clear();

        // Add a CheckMenuItem for each microphone
        for device in &self.input_devices {
            let is_selected = self.selected_mic_id.as_deref() == Some(&device.id);

            // CheckMenuItem::new(text, enabled, checked, accelerator)
            // - enabled: disabled while mixing (to prevent surprise device switches)
            // - checked: true for the currently selected mic
            let item = CheckMenuItem::new(
                &device.name,
                !is_mixing, // disabled while mixing — UX requirement
                is_selected,
                None, // no keyboard accelerator
            );

            // Store the mapping: MenuId -> device ID
            self.mic_menu_ids
                .push((item.id().clone(), device.id.clone()));
            self.mic_menu_items.push(item.clone());

            menu.append(&item)
                .context("Failed to append mic menu item")?;
        }

        if self.input_devices.is_empty() {
            // Show a disabled placeholder if no mics found
            let no_mics = MenuItem::new("(no microphones found)", false, None);
            menu.append(&no_mics)
                .context("Failed to append placeholder item")?;
        }

        // Separator between mic list and Quit
        //
        // RUST CONCEPT: `PredefinedMenuItem` — factory methods
        // `PredefinedMenuItem::separator()` is a "factory method" — a function
        // that creates a specific variant without exposing the internal construction.
        let separator = PredefinedMenuItem::separator();
        menu.append(&separator)
            .context("Failed to append separator")?;

        // Quit item
        let quit_item = MenuItem::new("Quit", true, None);
        self.quit_item_id = Some(quit_item.id().clone());
        menu.append(&quit_item)
            .context("Failed to append quit item")?;

        Ok(menu)
    }

    /// Create a 64x64 RGBA icon — a filled circle.
    ///
    /// - Grey (128,128,128) when idle
    /// - Green (0,200,0) when active/mixing
    ///
    /// RUST CONCEPT: Pure function (no `&self`)
    /// This is an "associated function" (like a static method in Python).
    /// It doesn't need `self` because the icon is computed solely from
    /// the `active` parameter.
    fn create_icon(active: bool) -> Icon {
        let (r, g, b): (u8, u8, u8) = if active {
            (0, 200, 0) // green = mixing
        } else {
            (128, 128, 128) // grey = idle
        };

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

    /// Start the audio pipeline — mic capture + loopback + mixing -> VB-Cable.
    fn start_mixing(&mut self) {
        let mic_id = match &self.selected_mic_id {
            Some(id) => id.clone(),
            None => {
                warn!("Cannot start mixing: no microphone selected");
                return;
            }
        };

        let vbcable_id = match &self.vbcable_id {
            Some(id) => id.clone(),
            None => {
                warn!("Cannot start mixing: VB-Cable not found");
                return;
            }
        };

        info!("Starting pipeline: mic={}, vbcable={}", mic_id, vbcable_id);

        match Pipeline::start(&mic_id, &vbcable_id) {
            Ok(pipeline) => {
                self.pipeline = Some(pipeline);
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

    /// Update the tray icon to reflect current state (idle vs mixing).
    fn update_icon(&self) {
        let active = self.pipeline.is_some();
        let icon = Self::create_icon(active);

        if let Some(tray) = &self.tray_icon {
            if let Err(e) = tray.set_icon(Some(icon)) {
                error!("Failed to update tray icon: {:?}", e);
            }

            // Update tooltip to show current state
            let tooltip = if active {
                "wasamix — MIXING"
            } else {
                "wasamix — idle (click to start)"
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
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

    /// Handle microphone selection from the context menu.
    ///
    /// UX REQUIREMENT: Selecting a mic does NOT auto-start mixing.
    /// It only updates the saved config. The user must left-click to start mixing.
    fn select_mic(&mut self, device_id: &str) {
        info!("Mic selected: {}", device_id);

        self.selected_mic_id = Some(device_id.to_string());

        // Persist the selection to disk
        self.config.mic_device_id = Some(device_id.to_string());
        if let Err(e) = self.config.save() {
            error!("Failed to save config: {:?}", e);
        }

        // Update the check marks on the menu items — uncheck all, then check
        // the selected one. This gives "radio button" behavior.
        for (menu_id, dev_id) in &self.mic_menu_ids {
            let is_selected = dev_id == device_id;
            // Find the corresponding CheckMenuItem and update it
            if let Some(item) = self.mic_menu_items.iter().find(|i| i.id() == menu_id) {
                item.set_checked(is_selected);
            }
        }
    }

    /// Handle a menu event (Quit or mic selection).
    ///
    /// RUST CONCEPT: Pattern matching on `MenuId`
    /// We compare the event's `id` against known IDs to determine which
    /// menu item was clicked. This is similar to a command dispatch pattern.
    fn handle_menu_event(&mut self, event: MenuEvent) {
        // Check if this is the Quit item
        if let Some(quit_id) = &self.quit_item_id
            && event.id == *quit_id
        {
            info!("Quit selected — shutting down");
            // Stop mixing before exiting (cleanup happens via Drop)
            self.stop_mixing();
            // The event loop will be exited in about_to_wait
            // We signal by dropping the tray icon
            self.tray_icon = None;
            return;
        }

        // Check if this is a mic selection item
        //
        // RUST CONCEPT: `.find()` returns `Option<&T>`
        // We search our ID->device mapping to find which mic was clicked.
        // `.cloned()` converts `Option<&String>` to `Option<String>`.
        let device_id = self
            .mic_menu_ids
            .iter()
            .find(|(menu_id, _)| *menu_id == event.id)
            .map(|(_, dev_id)| dev_id.clone());

        if let Some(device_id) = device_id {
            self.select_mic(&device_id);
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

        // Use Poll control flow so `about_to_wait` is called repeatedly.
        // This lets us poll for tray/menu events on each iteration.
        //
        // Note: In a real app you might prefer WaitUntil with a timer to save
        // CPU. For simplicity, we use Poll which keeps the loop spinning.
        event_loop.set_control_flow(ControlFlow::Poll);

        // Build the context menu
        let menu = self.build_menu().context("Failed to build context menu")?;
        self.menu = Some(menu.clone());

        // Create the tray icon
        let icon = Self::create_icon(false); // start in idle state
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("wasamix — idle (click to start)")
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

        // If the tray icon was dropped (user quit), exit the event loop.
        //
        // RUST CONCEPT: `is_none()` on Option
        // This is a simple boolean check — the idiomatic way to test
        // whether an Option holds a value.
        if self.tray_icon.is_none() {
            event_loop.exit();
        }
    }
}
