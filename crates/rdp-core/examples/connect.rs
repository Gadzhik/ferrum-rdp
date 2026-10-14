//! Headless smoke test: connect, wait for frames, save a screenshot.
//!
//! cargo run -p rdp-core --example connect -- /v:host /u:user /p:pass [out.png] [seconds]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rdp_core::*;

#[derive(Default)]
struct Probe {
    fb: Mutex<FrameBuffer>,
    finished: Mutex<Option<(u32, Option<String>)>>,
}

impl Handler for Probe {
    fn state(&self, state: State) {
        eprintln!("state: {state:?}");
    }
    fn finished(&self, error: u32, message: Option<String>) {
        *self.finished.lock().unwrap() = Some((error, message));
    }
    fn desktop_resize(&self, width: u32, height: u32) {
        eprintln!("desktop: {width}x{height}");
        self.fb.lock().unwrap().resize(width, height);
    }
    fn frame(&self, data: &[u8], stride: usize, width: u32, height: u32, rects: &[Rect]) {
        self.fb.lock().unwrap().blit(data, stride, width, height, rects);
    }
    fn verify_certificate(&self, info: CertificateInfo) -> CertVerdict {
        eprintln!("certificate {} for {}:{} accepted once", info.fingerprint, info.host, info.port);
        CertVerdict::AcceptOnce
    }
    fn clipboard_remote_offer(&self, mimes: Vec<String>) {
        eprintln!("remote clipboard offers {mimes:?}");
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = match args.last().and_then(|a| a.parse().ok()) {
        Some(s) => {
            args.pop();
            s
        }
        None => 5,
    };
    let out = match args.last() {
        Some(a) if a.ends_with(".png") => args.pop().unwrap(),
        _ => "screenshot.png".into(),
    };

    let probe = Arc::new(Probe::default());
    let config = Config { args, ..Default::default() };
    let session = Session::connect(&config, probe.clone()).expect("session");

    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut step = 0;
    while Instant::now() < deadline && probe.finished.lock().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(100));
        // exercise the input path: sweep the pointer diagonally
        step += 1;
        session.mouse_move(100 + step * 10, 100 + step * 5);
    }
    session.close();

    if let Some((code, msg)) = probe.finished.lock().unwrap().clone() {
        if code != 0 {
            eprintln!("disconnected: {}", msg.unwrap_or_default());
        }
    }

    let fb = probe.fb.lock().unwrap();
    if fb.width == 0 {
        eprintln!("no frame received");
        std::process::exit(1);
    }
    let file = std::fs::File::create(&out).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), fb.width, fb.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&fb.to_rgba()).unwrap();
    eprintln!("saved {}x{} to {out}", fb.width, fb.height);
}
