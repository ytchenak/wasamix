// "mod" declarations tell Rust which files belong to this module.
// Each file listed here becomes a submodule of `audio`.
// Other code can use them via `crate::audio::devices`, etc.

pub mod devices;
pub mod mixer;
pub mod capture;
pub mod pipeline;
