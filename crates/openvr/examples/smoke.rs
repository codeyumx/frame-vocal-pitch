//! Smoke test: if SteamVR is running, show a colour-cycling test overlay 0.6 m in front of the
//! HMD for 10 s, printing received events. Never launches SteamVR; exits 0 when it is not
//! running or when the OpenVR library is missing.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use fvp_openvr::{is_steamvr_running, FrameRef, Mat34, TextureSubmitter, Vr, HMD_INDEX};

const SIZE: u32 = 256;

fn main() -> ExitCode {
    match is_steamvr_running() {
        Ok(true) => {}
        Ok(false) => {
            println!("SteamVR not running");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            println!("OpenVR unavailable: {e}");
            return ExitCode::SUCCESS;
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("smoke test failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Premultiplied RGBA test card: solid colour with a white border and a translucent centre.
fn test_card(rgb: [u8; 3]) -> Vec<u8> {
    let mut px = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let border = x < 8 || y < 8 || x >= SIZE - 8 || y >= SIZE - 8;
            let centre = (96..160).contains(&x) && (96..160).contains(&y);
            let (c, a) = if border {
                ([255, 255, 255], 255u16)
            } else if centre {
                (rgb, 128)
            } else {
                (rgb, 255)
            };
            px.extend(c.iter().map(|&v| (v as u16 * a / 255) as u8));
            px.push(a as u8);
        }
    }
    px
}

fn run() -> fvp_openvr::Result<()> {
    let mut vr = Vr::init_overlay()?;
    println!(
        "SteamVR {} via {} (HMD worn: {})",
        vr.runtime_version(),
        vr.library_path().display(),
        vr.hmd_worn()
    );
    let overlay = vr.create_overlay("codeyumx.frame-vocal-pitch.smoke", "fvp smoke test")?;
    vr.set_width(overlay, 0.25)?;
    vr.set_sort_order(overlay, 10)?;
    vr.set_transform_tracked_device_relative(
        overlay,
        HMD_INDEX,
        &Mat34::translation(0.0, 0.0, -0.6),
    )?;

    let mut submitter = match TextureSubmitter::new(&vr) {
        Ok(s) => s,
        Err(e) => {
            vr.shutdown(None);
            return Err(e);
        }
    };
    let colours = [[46, 189, 89], [217, 67, 59], [224, 160, 48], [60, 120, 230]];
    let start = Instant::now();
    let mut shown = false;
    let mut last_colour = usize::MAX;
    let result = loop {
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_secs(10) {
            break Ok(());
        }
        let colour = (elapsed.as_millis() / 500) as usize % colours.len();
        if colour != last_colour {
            let rgba = test_card(colours[colour]);
            let frame = FrameRef {
                width: SIZE,
                height: SIZE,
                rgba: &rgba,
                premultiplied: true,
            };
            if let Err(e) = submitter.submit(&mut vr, overlay, &frame) {
                break Err(e);
            }
            last_colour = colour;
            if !shown {
                if let Err(e) = vr.show(overlay) {
                    break Err(e);
                }
                shown = true;
            }
        }
        let mut quit = false;
        for event in vr.poll_events() {
            println!("event: {event:?}");
            quit |= event == fvp_openvr::Event::Quit;
        }
        if quit {
            println!("SteamVR quit");
            break Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    vr.shutdown(Some(submitter));
    println!("clean shutdown");
    result
}
