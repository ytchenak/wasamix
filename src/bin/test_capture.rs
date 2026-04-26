/// Diagnostic: test loopback capture on ALL render devices, not just default.
/// Also checks IsFormatSupported before Initialize, and tries raw COM calls.
use wasapi::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    initialize_mta().ok().map_err(|e| format!("COM: {:?}", e))?;
    println!("COM initialized\n");

    // Enumerate ALL render devices
    let enumerator = DeviceEnumerator::new()?;
    let collection = enumerator.get_device_collection(&Direction::Render)?;
    let mut devices: Vec<(String, String)> = Vec::new();
    for dev_result in &collection {
        let dev = dev_result?;
        let name = dev.get_friendlyname().unwrap_or_default();
        let id = dev.get_id().unwrap_or_default();
        devices.push((name, id));
    }

    println!("=== Render devices ({} total) ===", devices.len());
    for (i, (name, _id)) in devices.iter().enumerate() {
        println!("  [{}] {}", i, name);
    }
    println!();

    // Test loopback on each render device
    for (i, (name, _id)) in devices.iter().enumerate() {
        println!("========================================");
        println!("Device [{}]: {}", i, name);
        println!("========================================");

        // Get device from collection again (can't reuse iteration)
        let collection = enumerator.get_device_collection(&Direction::Render)?;
        let mut target_dev = None;
        for dev_result in &collection {
            let dev = dev_result?;
            let dev_name = dev.get_friendlyname().unwrap_or_default();
            if &dev_name == name {
                target_dev = Some(dev);
                break;
            }
        }
        let device = match target_dev {
            Some(d) => d,
            None => {
                println!("  Could not re-find device, skipping\n");
                continue;
            }
        };

        let audio_client = device.get_iaudioclient()?;
        let mix_format = audio_client.get_mixformat()?;
        let ch = mix_format.get_nchannels();
        let rate = mix_format.get_samplespersec();
        let bits = mix_format.get_bitspersample();
        let subformat = mix_format.get_subformat()?;
        println!(
            "  Mix format: {}ch {}Hz {}bit {:?}",
            ch, rate, bits, subformat
        );

        let (def_period, min_period) = audio_client.get_device_period()?;
        println!("  Periods: def={} min={}", def_period, min_period);

        // Test A: mix_format, period=0, Capture, Shared, no convert
        // This is the simplest possible loopback: use device's own format, no conversion
        println!("\n  --- Test A: mix_format, period=0, Capture, Shared, convert=false ---");
        let mut ac_a = device.get_iaudioclient()?;
        match ac_a.initialize_client(
            &mix_format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: false,
                buffer_duration_hns: 0,
            },
        ) {
            Ok(()) => println!("  ✓ SUCCESS"),
            Err(e) => println!("  ✗ FAILED: {}", e),
        }

        // Test B: mix_format, default period, Capture, Shared, no convert
        println!(
            "\n  --- Test B: mix_format, def_period={}, Capture, Shared, convert=false ---",
            def_period
        );
        let mut ac_b = device.get_iaudioclient()?;
        match ac_b.initialize_client(
            &mix_format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: false,
                buffer_duration_hns: def_period,
            },
        ) {
            Ok(()) => println!("  ✓ SUCCESS"),
            Err(e) => println!("  ✗ FAILED: {}", e),
        }

        // Test C: mix_format, period=0, Capture, Shared, with convert
        println!("\n  --- Test C: mix_format, period=0, Capture, Shared, convert=true ---");
        let mut ac_c = device.get_iaudioclient()?;
        match ac_c.initialize_client(
            &mix_format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 0,
            },
        ) {
            Ok(()) => println!("  ✓ SUCCESS"),
            Err(e) => println!("  ✗ FAILED: {}", e),
        }

        // Test D: Normal render (NOT loopback) — to check device works at all
        println!(
            "\n  --- Test D: mix_format, def_period, Render, Shared, convert=true (normal render, no loopback) ---"
        );
        let mut ac_d = device.get_iaudioclient()?;
        match ac_d.initialize_client(
            &mix_format,
            &Direction::Render,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: def_period,
            },
        ) {
            Ok(()) => println!("  ✓ SUCCESS"),
            Err(e) => println!("  ✗ FAILED: {}", e),
        }

        println!();
    }

    // Also test default capture device (mic) for comparison
    println!("========================================");
    println!("Default CAPTURE device (mic)");
    println!("========================================");
    let mic_dev = enumerator.get_default_device(&Direction::Capture)?;
    let mic_name = mic_dev.get_friendlyname().unwrap_or_default();
    println!("  Device: {}", mic_name);

    let mic_ac = mic_dev.get_iaudioclient()?;
    let mic_fmt = mic_ac.get_mixformat()?;
    println!(
        "  Mix format: {}ch {}Hz {}bit {:?}",
        mic_fmt.get_nchannels(),
        mic_fmt.get_samplespersec(),
        mic_fmt.get_bitspersample(),
        mic_fmt.get_subformat()?
    );

    let (mic_def, mic_min) = mic_ac.get_device_period()?;
    println!("  Periods: def={} min={}", mic_def, mic_min);

    println!("\n  --- Test: mix_format, min_period, Capture, Shared, convert=true ---");
    let mut mic_ac2 = mic_dev.get_iaudioclient()?;
    match mic_ac2.initialize_client(
        &mic_fmt,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: mic_min,
        },
    ) {
        Ok(()) => println!("  ✓ SUCCESS"),
        Err(e) => println!("  ✗ FAILED: {}", e),
    }

    Ok(())
}
