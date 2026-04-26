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
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

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

/// Substrings that identify a VB-Audio virtual-cable endpoint. Order
/// doesn't matter; matching is case-insensitive `.contains()`.
///
/// Covers:
///   - `CABLE Input` / `CABLE Output` (standard cable)
///   - `CABLE-B Input` / `CABLE-B Output` / etc. (extra standalone cables)
///   - `CABLE In 16ch` / `CABLE Out 16ch` (Voicemeeter companion)
///
/// The `vb-audio` tag catches any future naming we don't anticipate.
const VBCABLE_NAMES: &[&str] = &[
    "cable input",
    "cable output",
    "cable-",
    "cable in ",
    "cable out ",
    "vb-audio",
];

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
    let enumerator = DeviceEnumerator::new().context("Failed to create device enumerator")?;

    // Enumerate capture (input) and render (output) devices
    for direction in [Direction::Capture, Direction::Render] {
        let collection = enumerator
            .get_device_collection(&direction)
            .context("Failed to get device collection")?;

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

/// Find VB-Cable's render (output) device — where we WRITE the mixed audio.
/// VB-Cable's render endpoint is named "CABLE Input" (confusing but correct:
/// it's an "input" to the virtual cable, but a "render" device from WASAPI's
/// perspective).
///
/// Preference order (for users who have the full VB-Audio suite installed):
/// 1. plain `CABLE Input`  — the standard 2-channel endpoint
/// 2. `CABLE-B Input` / `CABLE-C Input` etc. — other standalone cables
/// 3. `CABLE In 16ch`      — the 16-channel Voicemeeter companion, last-resort
pub fn find_vbcable(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    let render_cables: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|d| d.direction == DeviceDirection::Render && is_vbcable(&d.name))
        .collect();

    // Tier 1: exact "CABLE Input", excluding the 16-channel variant.
    if let Some(d) = render_cables
        .iter()
        .find(|d| {
            let l = d.name.to_lowercase();
            l.contains("cable input") && !l.contains("16ch")
        })
        .copied()
    {
        return Some(d);
    }
    // Tier 2: any other non-16ch CABLE endpoint (CABLE-B Input, etc.).
    if let Some(d) = render_cables
        .iter()
        .find(|d| !d.name.to_lowercase().contains("16ch"))
        .copied()
    {
        return Some(d);
    }
    // Tier 3: last resort — 16ch variant.
    render_cables.into_iter().next()
}

/// Filter to "real" output devices — render endpoints the user actually
/// listens through. Excludes VB-Cable endpoints (they're our destination,
/// not a sound source) and loopback aliases.
pub fn filter_render_devices(devices: &[DeviceInfo]) -> Vec<DeviceInfo> {
    devices
        .iter()
        .filter(|d| {
            d.direction == DeviceDirection::Render
                && !d.name.contains("[Loopback]")
                && !is_vbcable(&d.name)
        })
        .cloned()
        .collect()
}

/// ID of the current Windows-default render device. Used to tag the
/// corresponding entry in the system-source selector with "(Windows default)".
pub fn get_default_render_device_id_opt() -> Option<String> {
    initialize_mta().ok().ok()?;
    let enumerator = DeviceEnumerator::new().ok()?;
    let device = enumerator.get_default_device(&Direction::Render).ok()?;
    device.get_id().ok()
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

/// True if `name` looks like any VB-Audio virtual-cable endpoint (Input or
/// Output side, any cable letter, any channel width). Used both to keep
/// cables out of mic/output lists and to recognize them as destinations.
pub fn is_vbcable(name: &str) -> bool {
    let lower = name.to_lowercase();
    VBCABLE_NAMES.iter().any(|tag| lower.contains(tag))
}

/// Get the default output device's loopback — this captures system audio.
#[allow(dead_code)]
pub fn get_default_render_device_id() -> Result<String> {
    initialize_mta().ok().context("Failed to initialize COM")?;

    let enumerator = DeviceEnumerator::new().context("Failed to create device enumerator")?;
    let device = enumerator
        .get_default_device(&Direction::Render)
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

    #[test]
    fn test_find_vbcable_prefers_standard_over_16ch() {
        // Order is deliberately "16ch first" to prove find_vbcable scores,
        // not just picks the first match.
        let devices = vec![
            make_device("Speakers (Realtek)", DeviceDirection::Render),
            make_device(
                "CABLE In 16ch (VB-Audio Voicemeeter)",
                DeviceDirection::Render,
            ),
            make_device(
                "CABLE Input (VB-Audio Virtual Cable)",
                DeviceDirection::Render,
            ),
        ];
        let picked = find_vbcable(&devices).expect("should find a cable");
        assert!(
            picked.name.contains("CABLE Input") && !picked.name.contains("16ch"),
            "expected plain CABLE Input, got: {}",
            picked.name
        );
    }

    #[test]
    fn test_find_vbcable_falls_back_to_16ch() {
        // If only the 16ch variant is installed, use it.
        let devices = vec![
            make_device("Speakers (Realtek)", DeviceDirection::Render),
            make_device(
                "CABLE In 16ch (VB-Audio Voicemeeter)",
                DeviceDirection::Render,
            ),
        ];
        let picked = find_vbcable(&devices).expect("should find a cable");
        assert!(picked.name.contains("16ch"));
    }

    #[test]
    fn test_filter_render_excludes_all_vbcable_variants() {
        let devices = vec![
            make_device("Speakers (Realtek)", DeviceDirection::Render),
            make_device("Headset (Jabra Evolve2 65)", DeviceDirection::Render),
            make_device(
                "CABLE Input (VB-Audio Virtual Cable)",
                DeviceDirection::Render,
            ),
            make_device("CABLE-B Input (VB-Audio)", DeviceDirection::Render),
            make_device(
                "CABLE In 16ch (VB-Audio Voicemeeter)",
                DeviceDirection::Render,
            ),
            make_device("Speakers [Loopback]", DeviceDirection::Render),
        ];
        let result = filter_render_devices(&devices);
        let names: Vec<&str> = result.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Speakers (Realtek)", "Headset (Jabra Evolve2 65)"]
        );
    }

    #[test]
    fn test_is_vbcable_matches_hyphenated_variants() {
        assert!(is_vbcable("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_vbcable("CABLE-B Input (VB-Audio)"));
        assert!(is_vbcable("CABLE Output (VB-Audio)"));
        assert!(is_vbcable("CABLE In 16ch (VB-Audio Voicemeeter)"));
        assert!(!is_vbcable("Speakers (Realtek)"));
        assert!(!is_vbcable("Headset (Jabra Evolve2 65)"));
    }
}
