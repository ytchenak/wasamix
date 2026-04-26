//! WASAPI capture and render — opens audio streams for mic, loopback, and output.
//!
//! RUST CONCEPT: Threads and ownership transfer
//! =============================================
//! In Rust, each value has exactly ONE owner. When we spawn a thread, we need
//! to MOVE data into it (transfer ownership) or SHARE it via `Arc`.
//!
//! - `move` closure: Takes ownership of all captured variables. The spawning
//!   thread can no longer use them. This is how Rust prevents data races at
//!   compile time — no two threads can own the same data.
//!
//! - `Arc<AtomicBool>`: A thread-safe boolean flag. `Arc` = shared ownership
//!   across threads, `AtomicBool` = lock-free reads/writes. We use this as
//!   a "stop signal" — the main thread sets it to true, worker threads check
//!   it each loop iteration.
//!
//! - `Arc<Mutex<T>>`: Shared ownership + mutual exclusion. Only one thread
//!   can lock the Mutex at a time. We use this for the ring buffer so the
//!   capture thread can write and the pipeline thread can read safely.
//!
//! - `JoinHandle<()>`: A handle to a spawned thread. You can call `.join()`
//!   on it to wait for the thread to finish. The `()` means the thread
//!   returns nothing (like Python's `None`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};
use wasapi::{
    DeviceCollection, Direction, SampleType, ShareMode, WaveFormat,
};

use super::mixer::{convert_f32_to_mono_i16, RingBuffer, SAMPLE_RATE};

/// Try to initialize an audio client for loopback capture on a device.
/// Returns Ok(audio_client) if successful, or the error if it fails.
fn try_loopback_init(device: &wasapi::Device) -> Result<wasapi::AudioClient> {
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|e| anyhow::anyhow!("Failed to get audio client: {}", e))?;

    let mix_format = audio_client
        .get_mixformat()
        .map_err(|e| anyhow::anyhow!("Failed to get mix format: {}", e))?;

    audio_client
        .initialize_client(
            &mix_format,
            0,
            &Direction::Capture,
            &ShareMode::Shared,
            false,
        )
        .map_err(|e| anyhow::anyhow!("initialize_client failed: {}", e))?;

    Ok(audio_client)
}

/// Get a render device suitable for loopback capture.
/// Tries the default render device first. If it fails (common with Bluetooth
/// devices in certain states), falls back to other render devices.
fn get_loopback_device() -> Result<(wasapi::Device, wasapi::AudioClient)> {
    let default_dev = wasapi::get_default_device(&Direction::Render)
        .map_err(|e| anyhow::anyhow!("Failed to get default render device: {}", e))?;
    let default_name = default_dev.get_friendlyname().unwrap_or_default();

    match try_loopback_init(&default_dev) {
        Ok(ac) => {
            info!("Loopback: using default render device: {}", default_name);
            return Ok((default_dev, ac));
        }
        Err(e) => {
            warn!(
                "Default render device '{}' failed loopback init: {}. Trying alternatives...",
                default_name, e
            );
        }
    }

    let collection = DeviceCollection::new(&Direction::Render)
        .map_err(|e| anyhow::anyhow!("Failed to enumerate render devices: {}", e))?;

    let default_id = default_dev.get_id().unwrap_or_default();

    for dev_result in &collection {
        let dev = match dev_result {
            Ok(d) => d,
            Err(_) => continue,
        };
        let id = dev.get_id().unwrap_or_default();
        if id == default_id {
            continue;
        }
        let name = dev.get_friendlyname().unwrap_or_default();
        let lower = name.to_lowercase();
        if lower.contains("cable") {
            continue;
        }

        match try_loopback_init(&dev) {
            Ok(ac) => {
                info!("Loopback: fallback to render device: {}", name);
                return Ok((dev, ac));
            }
            Err(e) => {
                debug!("Render device '{}' also failed: {}", name, e);
            }
        }
    }

    anyhow::bail!(
        "No render device supports loopback capture. Default device '{}' error: device may be in \
         an exclusive state or have a driver issue (common with Bluetooth devices).",
        default_name
    )
}

// ─── helpers ────────────────────────────────────────────────────────────────

/// Find a device by its ID string within a given direction's collection.
///
/// The wasapi crate doesn't have a "get by ID" method, so we iterate through
/// all devices and match. This is only done once at thread startup, so the
/// cost is negligible.
fn find_device_by_id(device_id: &str, direction: &Direction) -> Result<wasapi::Device> {
    let collection = DeviceCollection::new(direction)
        .map_err(|e| anyhow::anyhow!("Failed to get device collection: {}", e))?;

    for device_result in &collection {
        let device = device_result
            .map_err(|e| anyhow::anyhow!("Failed to get device: {}", e))?;
        let id = device
            .get_id()
            .map_err(|e| anyhow::anyhow!("Failed to get device ID: {}", e))?;
        if id == device_id {
            return Ok(device);
        }
    }

    anyhow::bail!("Device not found with ID: {}", device_id)
}

/// Convert raw i16 (possibly multi-channel) audio bytes to mono i16 bytes.
///
/// RUST CONCEPT: Working with raw bytes
/// -------------------------------------
/// Audio data arrives as `&[u8]` but logically contains i16 samples (2 bytes
/// each). We use `chunks_exact(2)` to process pairs of bytes, then
/// `i16::from_le_bytes()` to interpret them as little-endian signed integers.
/// For multi-channel audio, we average all channels per frame to get mono.
fn convert_i16_to_mono(data: &[u8], channels: u16) -> Vec<u8> {
    if channels <= 1 {
        // Already mono — return as-is
        return data.to_vec();
    }

    let samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    let frame_count = samples.len() / channels as usize;
    let mut output = Vec::with_capacity(frame_count * 2);

    for frame in 0..frame_count {
        let mut sum: i32 = 0;
        for ch in 0..channels as usize {
            sum += samples[frame * channels as usize + ch] as i32;
        }
        let mono = (sum / channels as i32).clamp(-32768, 32767) as i16;
        output.extend_from_slice(&mono.to_le_bytes());
    }

    output
}

// ─── capture ────────────────────────────────────────────────────────────────

/// Start a capture thread for a device (mic or loopback).
///
/// RUST CONCEPT: `move` closures and thread spawning
/// --------------------------------------------------
/// `thread::spawn` takes a closure. The `move` keyword transfers ownership of
/// all captured variables (`device_id`, `buffer`, `stop_flag`, `is_loopback`)
/// INTO the closure. After the spawn, the calling thread can no longer use
/// those variables — the compiler enforces this.
///
/// For `Arc` values like `buffer` and `stop_flag`, `move` transfers one
/// reference count into the thread. The calling thread should `.clone()` the
/// Arc before spawning if it still needs access.
pub fn start_capture_thread(
    device_id: String,
    buffer: Arc<Mutex<RingBuffer>>,
    stop_flag: Arc<AtomicBool>,
    is_loopback: bool,
) -> Result<thread::JoinHandle<()>> {
    let label = if is_loopback { "loopback" } else { "mic" };
    info!("Starting {} capture thread for device {}", label, device_id);

    // RUST CONCEPT: `move` closure
    // The `move` keyword here means the closure takes ownership of
    // `device_id`, `buffer`, `stop_flag`, and `is_loopback`.
    let handle = thread::Builder::new()
        .name(format!("{}-capture", label))
        .spawn(move || {
            if let Err(e) = capture_loop(&device_id, &buffer, &stop_flag, is_loopback) {
                error!("{} capture loop failed: {:#}", label, e);
            }
            info!("{} capture thread exiting", label);
        })
        .context("Failed to spawn capture thread")?;

    Ok(handle)
}

/// The main capture loop — runs inside a dedicated thread.
///
/// Steps:
/// 1. Initialize COM (each thread needs its own COM init on Windows)
/// 2. Open the device by ID
/// 3. Get an AudioClient, query the device's preferred format
/// 4. Initialize for capture (with loopback flag if capturing system audio)
/// 5. Loop: read audio packets, convert to mono i16, write to ring buffer
/// 6. Clean up on exit
fn capture_loop(
    device_id: &str,
    buffer: &Arc<Mutex<RingBuffer>>,
    stop_flag: &Arc<AtomicBool>,
    is_loopback: bool,
) -> Result<()> {
    // Step 1: Initialize COM for this thread.
    // WASAPI is a COM-based API. Every thread that uses it must call this.
    wasapi::initialize_mta()
        .ok()
        .map_err(|e| anyhow::anyhow!("COM init failed: {:?}", e))?;

    // Step 2: Get the device and initialize the audio client.
    //
    // For loopback: the wasapi crate detects (Render device, Capture direction)
    // and adds the AUDCLNT_STREAMFLAGS_LOOPBACK flag automatically. We use a
    // fallback strategy because some devices (especially Bluetooth) may fail.
    //
    // For mic: we find the specific capture device by ID and initialize normally.
    let (device, audio_client) = if is_loopback {
        get_loopback_device()?
    } else {
        let dev = find_device_by_id(device_id, &Direction::Capture)?;
        let mut ac = dev
            .get_iaudioclient()
            .map_err(|e| anyhow::anyhow!("Failed to get audio client: {}", e))?;

        // Request our target format (mono i16 48kHz) instead of the device's
        // native format. With autoconvert, WASAPI resamples for us — critical
        // for devices like Bluetooth headsets that capture at 16kHz natively.
        let target_format = WaveFormat::new(
            16, 16, &SampleType::Int, SAMPLE_RATE as usize, 1, None,
        );

        ac.initialize_client(
            &target_format,
            0,
            &Direction::Capture,
            &ShareMode::Shared,
            true,
        )
        .map_err(|e| {
            let name = dev.get_friendlyname().unwrap_or_default();
            anyhow::anyhow!("Failed to initialize mic '{}': {}", name, e)
        })?;

        (dev, ac)
    };

    let name = device.get_friendlyname().unwrap_or_default();
    info!(
        "Opened {} device: {}",
        if is_loopback { "loopback" } else { "capture" },
        name
    );

    // Determine the stream format for reading audio data.
    // - Mic: we requested mono i16 48kHz with autoconvert, so data arrives in
    //   that format regardless of the device's native rate.
    // - Loopback: we used the device's mix format (typically stereo f32 48kHz).
    let (channels, sample_rate, bits_per_sample, sample_type, block_align) = if is_loopback {
        let format_client = device
            .get_iaudioclient()
            .map_err(|e| anyhow::anyhow!("Failed to get format query client: {}", e))?;
        let mix_format = format_client
            .get_mixformat()
            .map_err(|e| anyhow::anyhow!("Failed to get mix format: {}", e))?;
        (
            mix_format.get_nchannels(),
            mix_format.get_samplespersec(),
            mix_format.get_bitspersample(),
            mix_format.get_subformat()
                .map_err(|e| anyhow::anyhow!("Failed to get sample type: {}", e))?,
            mix_format.get_blockalign() as usize,
        )
    } else {
        // Mic uses our requested format: mono i16 48kHz
        (1u16, SAMPLE_RATE, 16u16, SampleType::Int, 2usize)
    };

    info!(
        "Stream format: {}ch, {}Hz, {}bit {:?}",
        channels, sample_rate, bits_per_sample, sample_type
    );

    // Set up event-driven buffering.
    // The wasapi crate always sets AUDCLNT_STREAMFLAGS_EVENTCALLBACK when
    // initializing, so we MUST create and set an event handle. The event is
    // signaled by WASAPI whenever a new buffer of audio data is ready.
    let h_event = audio_client
        .set_get_eventhandle()
        .map_err(|e| anyhow::anyhow!("Failed to set event handle: {}", e))?;

    let buffer_frame_count = audio_client
        .get_bufferframecount()
        .map_err(|e| anyhow::anyhow!("Failed to get buffer frame count: {}", e))?;

    // Step 5: Get the capture client and start the stream.
    let capture_client = audio_client
        .get_audiocaptureclient()
        .map_err(|e| anyhow::anyhow!("Failed to get capture client: {}", e))?;

    audio_client
        .start_stream()
        .map_err(|e| anyhow::anyhow!("Failed to start capture stream: {}", e))?;

    info!(
        "Capture stream started (buffer: {} frames, {} bytes/frame)",
        buffer_frame_count, block_align
    );

    // Allocate a read buffer large enough for the device's buffer.
    // We reuse this allocation every loop iteration to avoid repeated allocs.
    let mut read_buf = vec![0u8; buffer_frame_count as usize * block_align];

    // Step 6: Main capture loop.
    // RUST CONCEPT: `Ordering::Relaxed`
    // ---------------------------------
    // AtomicBool::load(Ordering::Relaxed) reads the flag without any memory
    // ordering guarantees beyond atomicity. This is fine for a simple "should
    // I stop?" flag — we don't need to synchronize other memory accesses.
    while !stop_flag.load(Ordering::Relaxed) {
        // Wait for WASAPI to signal that audio data is available.
        // Timeout of 200ms prevents the thread from hanging forever if
        // the device is unplugged or something goes wrong.
        match h_event.wait_for_event(200) {
            Ok(()) => {}
            Err(_) => {
                // Timeout — no data available. Just loop and check stop_flag.
                continue;
            }
        }

        // Read all available packets. In shared mode, WASAPI may deliver
        // data in multiple packets per event signal.
        loop {
            // Check how many frames are in the next packet.
            let frames_available = match capture_client.get_next_nbr_frames() {
                Ok(Some(0)) => break,
                Ok(Some(n)) => n,
                Ok(None) => break, // exclusive mode — shouldn't happen
                Err(e) => {
                    warn!("get_next_nbr_frames failed: {}", e);
                    break;
                }
            };

            // Ensure our read buffer is big enough.
            let needed = frames_available as usize * block_align;
            if read_buf.len() < needed {
                read_buf.resize(needed, 0);
            }

            // Read raw audio data from the device.
            let (frames_read, _flags) = match capture_client.read_from_device(&mut read_buf[..needed]) {
                Ok(result) => result,
                Err(e) => {
                    warn!("read_from_device failed: {}", e);
                    break;
                }
            };

            if frames_read == 0 {
                break;
            }

            let bytes_read = frames_read as usize * block_align;
            let raw_data = &read_buf[..bytes_read];

            // Convert to our internal format: mono i16.
            // The device may deliver float32 stereo (most common) or i16.
            let mono_i16 = match sample_type {
                SampleType::Float => {
                    // f32 (possibly multi-channel) -> mono i16
                    convert_f32_to_mono_i16(raw_data, channels)
                }
                SampleType::Int => {
                    // i16 (possibly multi-channel) -> mono i16
                    convert_i16_to_mono(raw_data, channels)
                }
            };

            // Write to the shared ring buffer.
            // RUST CONCEPT: `.lock().unwrap()`
            // --------------------------------
            // `Mutex::lock()` returns a `Result` because the lock could be
            // "poisoned" (if another thread panicked while holding it).
            // `.unwrap()` panics if that happened — reasonable here because
            // a poisoned mutex means something is very wrong.
            if let Ok(mut buf) = buffer.lock() {
                buf.write(&mono_i16);
            } else {
                error!("Ring buffer mutex poisoned — capture thread exiting");
                break;
            }
        }
    }

    // Step 7: Clean up — stop the stream.
    if let Err(e) = audio_client.stop_stream() {
        warn!("Failed to stop capture stream: {}", e);
    }
    info!("Capture stream stopped");

    Ok(())
}

// ─── render ─────────────────────────────────────────────────────────────────

/// Start a render thread that writes mixed audio to VB-Cable (or any output device).
///
/// The `get_data` callback is called each time the render loop needs audio.
/// It should return exactly the requested number of bytes of mono i16 audio.
///
/// RUST CONCEPT: `dyn FnMut() -> Vec<u8> + Send`
/// -----------------------------------------------
/// `dyn` means dynamic dispatch — we don't know the concrete type at compile
/// time, just that it implements `FnMut() -> Vec<u8>` (a callable that returns
/// bytes). `Send` means it can be transferred across threads. The `Arc<Mutex<>>`
/// wrapping ensures thread-safe access to this callback.
pub fn start_render_thread(
    device_id: String,
    get_data: Arc<Mutex<dyn FnMut(usize) -> Vec<u8> + Send>>,
    stop_flag: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>> {
    info!("Starting render thread for device {}", device_id);

    let handle = thread::Builder::new()
        .name("render".to_string())
        .spawn(move || {
            if let Err(e) = render_loop(&device_id, &get_data, &stop_flag) {
                error!("Render loop failed: {:#}", e);
            }
            info!("Render thread exiting");
        })
        .context("Failed to spawn render thread")?;

    Ok(handle)
}

/// The main render loop — runs inside a dedicated thread.
///
/// Writes mixed mono i16 audio to the output device (typically VB-Cable).
/// Uses the `autoconvert` feature so WASAPI handles any format conversion
/// between our mono i16 48kHz and whatever the device expects.
fn render_loop(
    device_id: &str,
    get_data: &Arc<Mutex<dyn FnMut(usize) -> Vec<u8> + Send>>,
    stop_flag: &Arc<AtomicBool>,
) -> Result<()> {
    // Step 1: COM init for this thread.
    wasapi::initialize_mta()
        .ok()
        .map_err(|e| anyhow::anyhow!("COM init failed: {:?}", e))?;

    // Step 2: Find the render device by ID.
    let device = find_device_by_id(device_id, &Direction::Render)?;
    let name = device.get_friendlyname().unwrap_or_default();
    info!("Opened render device: {} ({})", name, device_id);

    // Step 3: Set up audio client.
    // We want to write mono i16 at 48kHz. We use `autoconvert = true` so
    // WASAPI's built-in sample rate converter handles the mismatch between
    // our format and whatever the device natively supports.
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|e| anyhow::anyhow!("Failed to get audio client: {}", e))?;

    // Our desired output format: mono, 16-bit integer, 48kHz.
    let desired_format = WaveFormat::new(
        16,                  // storebits: 16 bits per sample
        16,                  // validbits: all 16 bits are valid
        &SampleType::Int,    // PCM integer
        SAMPLE_RATE as usize,
        1,                   // mono (1 channel)
        None,                // default channel mask
    );

    let (def_period, _min_period) = audio_client
        .get_periods()
        .map_err(|e| anyhow::anyhow!("Failed to get periods: {}", e))?;

    debug!("Render: initializing with desired format: {:?}", desired_format);

    // Step 4: Initialize for rendering with auto-conversion.
    // `convert: true` adds AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM so WASAPI
    // accepts our mono i16 format even if the device uses float32 stereo.
    audio_client
        .initialize_client(
            &desired_format,
            def_period,
            &Direction::Render,
            &ShareMode::Shared,
            true, // autoconvert — let WASAPI handle format conversion
        )
        .map_err(|e| anyhow::anyhow!("Failed to initialize render client: {}", e))?;

    // Event handle for buffer notifications.
    let h_event = audio_client
        .set_get_eventhandle()
        .map_err(|e| anyhow::anyhow!("Failed to set event handle: {}", e))?;

    let buffer_frame_count = audio_client
        .get_bufferframecount()
        .map_err(|e| anyhow::anyhow!("Failed to get buffer frame count: {}", e))?;

    let block_align = desired_format.get_blockalign() as usize; // 2 bytes (mono i16)

    // Step 5: Get the render client and start the stream.
    let render_client = audio_client
        .get_audiorenderclient()
        .map_err(|e| anyhow::anyhow!("Failed to get render client: {}", e))?;

    audio_client
        .start_stream()
        .map_err(|e| anyhow::anyhow!("Failed to start render stream: {}", e))?;

    info!(
        "Render stream started (buffer: {} frames, {} bytes/frame)",
        buffer_frame_count, block_align
    );

    // Step 6: Main render loop.
    while !stop_flag.load(Ordering::Relaxed) {
        // Wait for WASAPI to signal that it needs more audio data.
        match h_event.wait_for_event(200) {
            Ok(()) => {}
            Err(_) => {
                // Timeout — the device may have gone away. Check stop_flag
                // and try again.
                continue;
            }
        }

        // Find out how many frames of free space the device buffer has.
        let frames_available = match audio_client.get_available_space_in_frames() {
            Ok(n) => n as usize,
            Err(e) => {
                warn!("get_available_space_in_frames failed: {}", e);
                continue;
            }
        };

        if frames_available == 0 {
            continue;
        }

        let bytes_needed = frames_available * block_align;

        // Get mixed audio data via the callback.
        // RUST CONCEPT: Locking a Mutex around a closure
        // -----------------------------------------------
        // We lock the mutex, call the closure, then the lock is released
        // when `guard` goes out of scope (Rust's RAII / Drop pattern).
        let data = match get_data.lock() {
            Ok(mut guard) => guard(bytes_needed),
            Err(_) => {
                error!("get_data mutex poisoned — render thread exiting");
                break;
            }
        };

        // If we got less data than needed, pad with silence.
        let write_data = if data.len() < bytes_needed {
            let mut padded = data;
            padded.resize(bytes_needed, 0);
            padded
        } else {
            data
        };

        // Write to the device.
        if let Err(e) = render_client.write_to_device(frames_available, &write_data, None) {
            warn!("write_to_device failed: {}", e);
        }
    }

    // Step 7: Stop the stream.
    if let Err(e) = audio_client.stop_stream() {
        warn!("Failed to stop render stream: {}", e);
    }
    info!("Render stream stopped");

    Ok(())
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_i16_mono_passthrough() {
        // Mono data should pass through unchanged
        let input = vec![0x10, 0x20, 0x30, 0x40];
        let result = convert_i16_to_mono(&input, 1);
        assert_eq!(result, input);
    }

    #[test]
    fn test_convert_i16_stereo_to_mono() {
        // Two frames of stereo: (100, 200), (300, 400)
        // Expected mono: avg(100,200)=150, avg(300,400)=350
        let mut input = Vec::new();
        input.extend_from_slice(&100i16.to_le_bytes());
        input.extend_from_slice(&200i16.to_le_bytes());
        input.extend_from_slice(&300i16.to_le_bytes());
        input.extend_from_slice(&400i16.to_le_bytes());

        let result = convert_i16_to_mono(&input, 2);
        let s0 = i16::from_le_bytes([result[0], result[1]]);
        let s1 = i16::from_le_bytes([result[2], result[3]]);
        assert_eq!(s0, 150);
        assert_eq!(s1, 350);
    }

    #[test]
    fn test_convert_i16_empty() {
        let result = convert_i16_to_mono(&[], 2);
        assert!(result.is_empty());
    }
}
