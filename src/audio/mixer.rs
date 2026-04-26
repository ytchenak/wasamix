//! Audio mixing utilities: ring buffer for thread-safe audio buffering,
//! and sample mixing with clamping.
//!
//! RUST CONCEPT: `Arc<Mutex<T>>` — shared ownership + mutual exclusion
//! --------------------------------------------------------------------
//! In Python, threads can freely share mutable data (the GIL "sort of"
//! protects you). In Rust, the compiler REFUSES to let two threads access
//! the same data unless you prove it's safe:
//!
//!   `Mutex<T>` — a lock that ensures only one thread accesses T at a time.
//!   `Arc<T>`   — "Atomic Reference Count" — lets multiple threads OWN the
//!                same data. When the last Arc is dropped, the data is freed.
//!
//! So `Arc<Mutex<RingBuffer>>` means: multiple threads share ownership of a
//! locked ring buffer. The compiler checks this at compile time — if you
//! forget the Arc or Mutex, your code won't compile. No race conditions!

use std::sync::{Arc, Mutex};

/// Audio format constants — 48kHz mono 16-bit (2 bytes per sample).
pub const SAMPLE_RATE: u32 = 48000;
#[allow(dead_code)]
pub const CHANNELS: u16 = 1;
pub const BITS_PER_SAMPLE: u16 = 16;
pub const BYTES_PER_SAMPLE: usize = (BITS_PER_SAMPLE / 8) as usize;
#[allow(dead_code)]
pub const CHUNK_FRAMES: usize = 1024;
#[allow(dead_code)]
pub const CHUNK_BYTES: usize = CHUNK_FRAMES * BYTES_PER_SAMPLE;

/// A circular byte buffer for audio data.
///
/// RUST CONCEPT: `struct` = data, `impl` = behavior
/// -------------------------------------------------
/// Unlike Python classes where data and methods live together, Rust
/// separates them. `struct` defines the fields. `impl` adds methods.
/// This makes it clear what data a type holds vs. what it can do.
pub struct RingBuffer {
    buf: Vec<u8>,
    capacity: usize,
    write_pos: usize,
    size: usize,
}

impl RingBuffer {
    /// Create a new ring buffer with the given capacity in bytes.
    pub fn new(capacity: usize) -> Self {
        // `vec![0u8; capacity]` creates a Vec of `capacity` zero bytes.
        // In Python: [0] * capacity
        RingBuffer {
            buf: vec![0u8; capacity],
            capacity,
            write_pos: 0,
            size: 0,
        }
    }

    /// Write data into the buffer. If the buffer is full, oldest data is overwritten.
    ///
    /// RUST CONCEPT: `&[u8]` — a "slice"
    /// -----------------------------------
    /// `&[u8]` is a borrowed reference to a contiguous sequence of bytes.
    /// It's like Python's `bytes` but doesn't own the data — it just points
    /// to memory owned by someone else. This is Rust "borrowing" in action.
    pub fn write(&mut self, data: &[u8]) {
        let n = data.len();
        if n == 0 {
            return;
        }

        if n >= self.capacity {
            // Data is larger than buffer — keep only the newest part
            let start = n - self.capacity;
            self.buf.copy_from_slice(&data[start..]);
            self.write_pos = 0;
            self.size = self.capacity;
            return;
        }

        let end = self.write_pos + n;
        if end <= self.capacity {
            self.buf[self.write_pos..end].copy_from_slice(data);
        } else {
            let first = self.capacity - self.write_pos;
            self.buf[self.write_pos..].copy_from_slice(&data[..first]);
            self.buf[..n - first].copy_from_slice(&data[first..]);
        }

        self.write_pos = (self.write_pos + n) % self.capacity;
        self.size = (self.size + n).min(self.capacity);
    }

    /// Read `nbytes` from the buffer. Pads with silence (zeros) if not enough data.
    ///
    /// RUST CONCEPT: `Vec<u8>` — an owned, growable byte array
    /// --------------------------------------------------------
    /// Unlike `&[u8]` (borrowed), `Vec<u8>` OWNS its data. When you return
    /// a Vec from a function, ownership transfers to the caller (this is
    /// Rust's "move" semantics). No copying needed — just pointer handoff.
    pub fn read(&mut self, nbytes: usize) -> Vec<u8> {
        let available = nbytes.min(self.size);

        if available == 0 {
            return vec![0u8; nbytes];
        }

        let read_pos = (self.write_pos + self.capacity - self.size) % self.capacity;

        let mut result = Vec::with_capacity(nbytes);
        if read_pos + available <= self.capacity {
            result.extend_from_slice(&self.buf[read_pos..read_pos + available]);
        } else {
            let first = self.capacity - read_pos;
            result.extend_from_slice(&self.buf[read_pos..]);
            result.extend_from_slice(&self.buf[..available - first]);
        }

        self.size -= available;

        // Pad with silence if we didn't have enough data
        result.resize(nbytes, 0);
        result
    }
}

/// Create a shared ring buffer that multiple threads can access.
///
/// Returns `Arc<Mutex<RingBuffer>>` — the caller can `.clone()` the Arc
/// to give another thread its own "handle" to the same buffer.
pub fn new_shared_buffer(capacity: usize) -> Arc<Mutex<RingBuffer>> {
    Arc::new(Mutex::new(RingBuffer::new(capacity)))
}

/// Mix two mono i16 audio buffers by summing samples and clamping.
///
/// RUST CONCEPT: Working with raw bytes as typed data
/// --------------------------------------------------
/// Audio arrives as `&[u8]` (raw bytes) but we need to treat it as i16
/// samples. We use `i16::from_le_bytes()` to convert pairs of bytes into
/// signed 16-bit integers (little-endian, which is what Windows uses).
pub fn mix_samples(mic: &[u8], loopback: &[u8]) -> Vec<u8> {
    if mic.is_empty() && loopback.is_empty() {
        return Vec::new();
    }

    let max_len = mic.len().max(loopback.len());
    // Ensure even number of bytes (i16 = 2 bytes)
    let sample_count = max_len / BYTES_PER_SAMPLE;
    let mut output = Vec::with_capacity(sample_count * BYTES_PER_SAMPLE);

    for i in 0..sample_count {
        let byte_offset = i * BYTES_PER_SAMPLE;

        // Read i16 sample from mic buffer, or 0 if past end
        let mic_sample = if byte_offset + 1 < mic.len() {
            i16::from_le_bytes([mic[byte_offset], mic[byte_offset + 1]])
        } else {
            0i16
        };

        // Read i16 sample from loopback buffer, or 0 if past end
        let loop_sample = if byte_offset + 1 < loopback.len() {
            i16::from_le_bytes([loopback[byte_offset], loopback[byte_offset + 1]])
        } else {
            0i16
        };

        // Sum as i32 to avoid overflow, then clamp back to i16 range.
        // `.clamp()` is a built-in method on numeric types — neat!
        let mixed = (mic_sample as i32 + loop_sample as i32).clamp(-32768, 32767) as i16;

        output.extend_from_slice(&mixed.to_le_bytes());
    }

    output
}

/// Convert audio from any format to mono i16 at the target sample rate.
///
/// RUST CONCEPT: `match` — exhaustive pattern matching
/// ---------------------------------------------------
/// `match` is like Python's match/case but the compiler ensures you handle
/// ALL cases. If you add a new variant to an enum and forget to handle it,
/// your code won't compile. This prevents bugs from unhandled cases.
pub fn convert_f32_to_mono_i16(data: &[u8], channels: u16) -> Vec<u8> {
    let f32_samples: Vec<f32> = data
        .chunks_exact(4) // f32 = 4 bytes
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    let frame_count = f32_samples.len() / channels as usize;
    let mut output = Vec::with_capacity(frame_count * BYTES_PER_SAMPLE);

    for frame in 0..frame_count {
        // Average all channels to get mono
        let mut sum = 0.0f32;
        for ch in 0..channels as usize {
            sum += f32_samples[frame * channels as usize + ch];
        }
        let mono = sum / channels as f32;

        // Convert float [-1.0, 1.0] to i16 [-32768, 32767]
        let sample = (mono * 32768.0).clamp(-32768.0, 32767.0) as i16;
        output.extend_from_slice(&sample.to_le_bytes());
    }

    output
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_write_and_read() {
        let mut buf = RingBuffer::new(1024);
        buf.write(&[1, 2, 3, 4]);
        let result = buf.read(4);
        assert_eq!(result, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_ring_buffer_read_pads_with_silence() {
        let mut buf = RingBuffer::new(1024);
        buf.write(&[1, 2]);
        let result = buf.read(4);
        assert_eq!(result, vec![1, 2, 0, 0]);
    }

    #[test]
    fn test_ring_buffer_read_empty_returns_silence() {
        let mut buf = RingBuffer::new(1024);
        let result = buf.read(4);
        assert_eq!(result, vec![0, 0, 0, 0]);
    }

    #[test]
    fn test_ring_buffer_wraps_around() {
        let mut buf = RingBuffer::new(8);
        buf.write(&[1, 2, 3, 4, 5, 6]);
        buf.read(6);
        buf.write(&[7, 8, 9, 10]);
        let result = buf.read(4);
        assert_eq!(result, vec![7, 8, 9, 10]);
    }

    #[test]
    fn test_ring_buffer_overflow_drops_oldest() {
        let mut buf = RingBuffer::new(4);
        buf.write(&[1, 2, 3, 4]);
        buf.write(&[5, 6]);
        let result = buf.read(4);
        assert_eq!(result, vec![3, 4, 5, 6]);
    }

    #[test]
    fn test_mix_simple_addition() {
        let a = 100i16.to_le_bytes();
        let b = 300i16.to_le_bytes();
        let mic = [a[0], a[1], 200i16.to_le_bytes()[0], 200i16.to_le_bytes()[1]];
        let loopback = [b[0], b[1], 400i16.to_le_bytes()[0], 400i16.to_le_bytes()[1]];
        let result = mix_samples(&mic, &loopback);
        let s0 = i16::from_le_bytes([result[0], result[1]]);
        let s1 = i16::from_le_bytes([result[2], result[3]]);
        assert_eq!(s0, 400);
        assert_eq!(s1, 600);
    }

    #[test]
    fn test_mix_clamp_positive() {
        let a = 30000i16.to_le_bytes();
        let b = 30000i16.to_le_bytes();
        let result = mix_samples(&a, &b);
        let s = i16::from_le_bytes([result[0], result[1]]);
        assert_eq!(s, 32767);
    }

    #[test]
    fn test_mix_clamp_negative() {
        let a = (-30000i16).to_le_bytes();
        let b = (-30000i16).to_le_bytes();
        let result = mix_samples(&a, &b);
        let s = i16::from_le_bytes([result[0], result[1]]);
        assert_eq!(s, -32768);
    }

    #[test]
    fn test_mix_empty() {
        assert!(mix_samples(&[], &[]).is_empty());
    }

    #[test]
    fn test_convert_f32_stereo_to_mono() {
        // Two frames of stereo: (0.5, -0.5), (1.0, 0.0)
        let mut data = Vec::new();
        data.extend_from_slice(&0.5f32.to_le_bytes());
        data.extend_from_slice(&(-0.5f32).to_le_bytes());
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data.extend_from_slice(&0.0f32.to_le_bytes());

        let result = convert_f32_to_mono_i16(&data, 2);
        let s0 = i16::from_le_bytes([result[0], result[1]]);
        let s1 = i16::from_le_bytes([result[2], result[3]]);
        // Frame 0: avg(0.5, -0.5) = 0.0 -> 0
        // Frame 1: avg(1.0, 0.0) = 0.5 -> 16384
        assert_eq!(s0, 0);
        assert_eq!(s1, 16384);
    }
}
