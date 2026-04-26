//! Audio pipeline — orchestrates mic capture + loopback capture + mixing + output.
//!
//! RUST CONCEPT: Ownership and resource management
//! ------------------------------------------------
//! In Python, the garbage collector cleans up resources eventually.
//! In Rust, resources are cleaned up IMMEDIATELY when the owner goes out
//! of scope. This is called RAII (Resource Acquisition Is Initialization).
//!
//! Our `Pipeline` struct owns the thread handles and stop flag. When
//! `Pipeline::stop()` is called (or Pipeline is dropped), it signals all
//! threads to stop and waits for them to finish. No resource leaks!

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use tracing::info;

use super::capture::{start_capture_thread, start_render_thread};
use super::mixer::{mix_samples, new_shared_buffer, SAMPLE_RATE, BYTES_PER_SAMPLE};

/// Buffer 2 seconds of audio
const BUFFER_CAPACITY: usize = SAMPLE_RATE as usize * BYTES_PER_SAMPLE * 2;

/// The audio pipeline — owns all threads and shared state.
///
/// RUST CONCEPT: `Option<T>` for nullable fields
/// -----------------------------------------------
/// `Option<JoinHandle>` is either `Some(handle)` or `None`.
/// We start with `None` and fill them in `start()`.
pub struct Pipeline {
    stop_flag: Arc<AtomicBool>,
    mic_thread: Option<thread::JoinHandle<()>>,
    loopback_thread: Option<thread::JoinHandle<()>>,
    render_thread: Option<thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Start the audio pipeline: mic capture + loopback capture + mixer/render.
    ///
    /// RUST CONCEPT: `bail!` macro
    /// ----------------------------
    /// `bail!("message")` is shorthand for `return Err(anyhow!("message"))`.
    /// It's from the `anyhow` crate — a convenient way to return errors.
    pub fn start(mic_device_id: &str, vbcable_device_id: &str) -> Result<Self> {
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Create shared ring buffers for mic and loopback audio
        let mic_buffer = new_shared_buffer(BUFFER_CAPACITY);
        let loopback_buffer = new_shared_buffer(BUFFER_CAPACITY);

        // Start mic capture thread
        let mic_thread = start_capture_thread(
            mic_device_id.to_string(),
            Arc::clone(&mic_buffer),
            Arc::clone(&stop_flag),
            false,
        )?;

        // Start loopback capture thread.
        // The device_id is unused for loopback — the thread selects the best
        // available render device internally (with fallback if default fails).
        let loopback_thread = start_capture_thread(
            String::new(),
            Arc::clone(&loopback_buffer),
            Arc::clone(&stop_flag),
            true,
        )?;

        // Create the mixer function that the render thread will call.
        // NOTE: The render thread calls get_data(bytes_needed) — it passes
        // the number of bytes it needs. We read that many bytes from each
        // ring buffer and mix them.
        let mic_buf_for_mixer = Arc::clone(&mic_buffer);
        let loop_buf_for_mixer = Arc::clone(&loopback_buffer);
        let mixer_fn: Arc<Mutex<dyn FnMut(usize) -> Vec<u8> + Send>> =
            Arc::new(Mutex::new(move |bytes_needed: usize| {
                let mic_data = mic_buf_for_mixer.lock().unwrap().read(bytes_needed);
                let loop_data = loop_buf_for_mixer.lock().unwrap().read(bytes_needed);
                mix_samples(&mic_data, &loop_data)
            }));

        // Start render thread — writes mixed audio to VB-Cable
        let render_thread = start_render_thread(
            vbcable_device_id.to_string(),
            mixer_fn,
            Arc::clone(&stop_flag),
        )?;

        info!("Pipeline started: mic={} vbcable={}", mic_device_id, vbcable_device_id);

        Ok(Pipeline {
            stop_flag,
            mic_thread: Some(mic_thread),
            loopback_thread: Some(loopback_thread),
            render_thread: Some(render_thread),
        })
    }

    /// Stop the pipeline — signals all threads and waits for them.
    ///
    /// RUST CONCEPT: `.take()` on Option
    /// -----------------------------------
    /// `self.mic_thread.take()` extracts the value from `Some(handle)`,
    /// leaving `None` in its place. This ensures we can only join once.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);

        if let Some(h) = self.mic_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.loopback_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.render_thread.take() {
            let _ = h.join();
        }

        info!("Pipeline stopped");
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        !self.stop_flag.load(Ordering::Relaxed)
    }
}

/// RUST CONCEPT: `Drop` trait — automatic cleanup
/// -----------------------------------------------
/// `Drop` is like Python's `__del__` but RELIABLE — it's called exactly when
/// the value goes out of scope. Here we ensure threads are stopped even if
/// the caller forgets to call `stop()`.
impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop();
    }
}