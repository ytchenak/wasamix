/// Integration test: starts the full audio pipeline for 5 seconds.
/// Tests mic capture + loopback capture + mixing + VB-Cable output.
use std::thread;
use std::time::Duration;

use wasamix::audio::devices::{enumerate_devices, filter_input_devices, find_vbcable};
use wasamix::audio::pipeline::Pipeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Pipeline Integration Test ===\n");

    let devices = enumerate_devices()?;

    let vbcable = find_vbcable(&devices).ok_or("VB-Cable not found")?;
    println!("VB-Cable: {} ({})", vbcable.name, vbcable.id);

    let inputs = filter_input_devices(&devices);
    if inputs.is_empty() {
        return Err("No input devices found".into());
    }
    let mic = &inputs[0];
    println!("Mic: {} ({})", mic.name, mic.id);

    println!("\nStarting pipeline...");
    let mut pipeline = Pipeline::start(&mic.id, &vbcable.id)?;
    println!("Pipeline started! Mixing for 5 seconds...\n");

    thread::sleep(Duration::from_secs(5));

    println!("Stopping pipeline...");
    pipeline.stop();
    println!("Pipeline stopped. Test complete!");

    Ok(())
}
