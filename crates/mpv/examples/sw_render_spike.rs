//! Phase-2 spike 1 (make-or-break): prove libmpv's software render API yields
//! real pixels off-screen, with no window and no GL context — the frame a
//! sandboxed decoder would hand to the host (docs/research/renderer-sandbox-phase2.md).
//!
//! Usage:
//!   cargo run -p mpv --example sw_render_spike -- <libmpv.dll> <video> <out.ppm> [W H]
//!
//! Writes one rendered frame as a binary PPM (P6) so it can be inspected.

use std::io::Write;
use std::time::{Duration, Instant};

use mpv::{Api, Player, RenderContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dll = args.next().expect("arg 1: path to libmpv-2.dll");
    let video = args.next().expect("arg 2: input video");
    let out = args.next().expect("arg 3: output .ppm");
    let width: i32 = args.next().map_or(640, |s| s.parse().unwrap());
    let height: i32 = args.next().map_or(360, |s| s.parse().unwrap());

    let api = Api::load_from(&dll)?;
    // No --wid: the render API is the video output. hwdec=no keeps decoding on
    // the CPU so this works on a headless box.
    let player = Player::new(
        api,
        &[("vo", "libmpv"), ("hwdec", "no"), ("audio", "no"), ("terminal", "no")],
    )?;
    let render = RenderContext::new_sw(&player)?;

    player.command(&["loadfile", &video])?;

    let stride = (width as usize) * 4;
    let mut buffer = vec![0u8; stride * (height as usize)];

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rendered = false;
    while Instant::now() < deadline {
        let _ = player.wait_event(0.05); // pump the event queue so mpv advances
        if render.frame_ready() {
            render.render_sw(width, height, "rgb0", stride, &mut buffer)?;
            rendered = true;
            break;
        }
    }
    if !rendered {
        return Err("no frame became ready within 10s".into());
    }

    // rgb0 = R,G,B,pad per pixel -> write RGB as PPM P6.
    let mut file = std::io::BufWriter::new(std::fs::File::create(&out)?);
    write!(file, "P6\n{width} {height}\n255\n")?;
    for pixel in buffer.chunks_exact(4) {
        file.write_all(&pixel[0..3])?;
    }
    file.flush()?;
    println!("wrote {out} ({width}x{height})");
    Ok(())
}
