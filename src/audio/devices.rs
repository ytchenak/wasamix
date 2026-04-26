//! Device enumeration — finds microphones, VB-Cable, and the loopback device.
//!
//! RUST CONCEPT: `String` vs `&str`
//! ---------------------------------
//! `String` is an owned, heap-allocated string — like Python's `str`.
//! `&str` is a borrowed reference to a string — a view into someone else's data.
//!
//! Rule of thumb: functions TAKE `&str` (borrow) and RETURN `String` (own).
//! This way the caller keeps ownership, and the function just peeks at the data.
//!
//! RUST CONCEPT: Iterators and closures
//! -------------------------------------
//! `.iter().filter(|d| ...).map(|d| ...).collect()` is Rust's iterator chain —
//! similar to Python's list comprehensions but composable and lazy (only
//! computes values when consumed by `.collect()`).

use anyhow::{Context, Result};
use tracing::info;
use wasapi::{DeviceCollection, Direction, initialize_mta};

/// Information about an audio device — our simplified view.
///
/// RUST CONCEPT: `#[derive(Clone, Debug)]`
/// ----------------------------------------
/// `Clone` lets you call `.clone()` to make a deep copy.
/// `Debug` lets you print the struct with `{:?}` formatting.
/// These are "traits" — like Python's `__repr__` and copy support,
/// but generated automatically by the compiler.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub direction: DeviceDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeviceDirection {
    Capture,
    Render,
}

const VBCABLE_NAMES: &[&str] = &["cable input", "cable output", "cable in "];

/// Enumerate all active audio devices.
///
/// RUST CONCEPT: `-> Result<Vec<DeviceInfo>>`
/// -------------------------------------------
/// This returns either `Ok(vec_of_devices)` or `Err(some_error)`.
/// The `?` operator inside propagates errors upward automatically.
pub fn enumerate_devices() -> Result<Vec<DeviceInfo>> {
    // Initialize COM for Windows audio — WASAPI requires this
    // RUST CONCEPT: `.ok()` converts HRESULT to Result<()>
    initialize_mta().ok().context("Failed to initialize COM")?;

    let mut devices = Vec::new();

    // Enumerate capture (input) devices
    for direction in [Direction::Capture, Direction::Render] {
        let collection =
            DeviceCollection::new(&direction).context("Failed to get device collection")?;

        // RUST CONCEPT: Iterator pattern
        // ------------------------------
        // The `&collection` reference lets us iterate without consuming the collection.
        // Each item is a Result<Device>, so we use `?` to propagate errors.
        for device_result in &collection {
            let device = device_result?;
            let name = device.get_friendlyname().unwrap_or_default();
            let id = device.get_id().unwrap_or_default();

            let dir = match direction {
                Direction::Capture => DeviceDirection::Capture,
                Direction::Render => DeviceDirection::Render,
            };

            devices.push(DeviceInfo {
                id,
                name,
                direction: dir,
            });
        }
    }

    Ok(devices)
}

/// Find VB-Cable's render (output) device index — this is where we WRITE the mixed audio.
/// VB-Cable's render endpoint is named "CABLE Input" (confusing but correct:
/// it's an "input" to the virtual cable, but a "render" device from WASAPI's perspective).
pub fn find_vbcable(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devices.iter().find(|d| {
        d.direction == DeviceDirection::Render && d.name.to_lowercase().contains("cable input")
    })
}

/// Filter to only real microphones — excludes VB-Cable endpoints and loopback devices.
///
/// RUST CONCEPT: Closures and `.filter()`
/// ----------------------------------------
/// `|d|` is a closure (anonymous function) — like Python's `lambda d:`.
/// `.filter()` keeps only items where the closure returns true.
/// `.cloned()` converts `&DeviceInfo` references to owned `DeviceInfo` values.
pub fn filter_input_devices(devices: &[DeviceInfo]) -> Vec<DeviceInfo> {
    devices
        .iter()
        .filter(|d| {
            d.direction == DeviceDirection::Capture
                && !d.name.contains("[Loopback]")
                && !is_vbcable(&d.name)
        })
        .cloned()
        .collect()
}

fn is_vbcable(name: &str) -> bool {
    let lower = name.to_lowercase();
    VBCABLE_NAMES.iter().any(|tag| lower.contains(tag))
}

/// Get the default output device's loopback — this captures system audio.
#[allow(dead_code)]
pub fn get_default_render_device_id() -> Result<String> {
    initialize_mta().ok().context("Failed to initialize COM")?;

    let device = wasapi::get_default_device(&Direction::Render)
        .context("Failed to get default render device")?;
    let id = device.get_id().context("Failed to get device ID")?;
    let name = device.get_friendlyname().unwrap_or_default();
    info!("Default render device: {} ({})", name, id);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device(name: &str, dir: DeviceDirection) -> DeviceInfo {
        DeviceInfo {
            id: format!("id-{}", name),
            name: name.to_string(),
            direction: dir,
        }
    }

    #[test]
    fn test_find_vbcable() {
        let devices = vec![
            make_device("Speakers (Realtek)", DeviceDirection::Render),
            make_device(
                "CABLE Input (VB-Audio Virtual Cable)",
                DeviceDirection::Render,
            ),
            make_device("Microphone (Realtek)", DeviceDirection::Capture),
        ];
        let result = find_vbcable(&devices);
        assert!(result.is_some());
        assert!(result.unwrap().name.contains("CABLE Input"));
    }

    #[test]
    fn test_find_vbcable_missing() {
        let devices = vec![make_device("Speakers (Realtek)", DeviceDirection::Render)];
        assert!(find_vbcable(&devices).is_none());
    }

    #[test]
    fn test_filter_input_excludes_vbcable_and_loopback() {
        let devices = vec![
            make_device("Microphone (Realtek)", DeviceDirection::Capture),
            make_device("CABLE Output (VB-Audio)", DeviceDirection::Capture),
            make_device("Speakers [Loopback]", DeviceDirection::Capture),
            make_device("Speakers (Realtek)", DeviceDirection::Render),
        ];
        let result = filter_input_devices(&devices);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Microphone (Realtek)");
    }
}
