//! D1's bench: the resource written raw and deflated per blob, each loaded from a file by the
//! app's own `Store::load` (read + parse + inflate + CRC), 10 warm-ups then 100 runs, median and
//! p90; the clock's overhead read first. The rule: deflate unless its load exceeds 50 ms.

use crate::store::Built;
use ondar_map::format::{Encoding, Store};
use std::time::Instant;

pub const RULE_MS: f64 = 50.0;

#[derive(Clone, Debug)]
pub struct Bench {
    pub clock_ns: f64,
    /// (bytes, median ms, p90 ms) for raw, then deflated.
    pub raw: (usize, f64, f64),
    pub deflate: (usize, f64, f64),
}

impl Bench {
    pub fn choice(&self) -> Encoding {
        if self.deflate.1 > RULE_MS {
            Encoding::Raw
        } else {
            Encoding::Deflate
        }
    }
}

fn time_load(path: &std::path::Path) -> Result<(f64, f64), String> {
    let mut v = Vec::with_capacity(100);
    for i in 0..110 {
        let t = Instant::now();
        let b = std::fs::read(path).map_err(|e| e.to_string())?;
        let s = Store::load(&b).map_err(|e| e.to_string())?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        std::hint::black_box(s.raw_bytes());
        if i >= 10 {
            v.push(ms);
        }
    }
    v.sort_by(f64::total_cmp);
    Ok((v[49] / 2.0 + v[50] / 2.0, v[89]))
}

pub fn run(built: &Built) -> Result<Bench, String> {
    let clock_ns = {
        let t = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(Instant::now());
        }
        t.elapsed().as_secs_f64() * 1e9 / 1000.0
    };
    let dir = std::env::temp_dir().join(format!("ondar-map-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for enc in [Encoding::Raw, Encoding::Deflate] {
        let bytes = built.write(enc).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{enc:?}.ondarmap"));
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        let (median, p90) = time_load(&path)?;
        out.push((bytes.len(), median, p90));
    }
    std::fs::remove_dir_all(&dir).ok();
    let b = Bench {
        clock_ns,
        raw: out[0],
        deflate: out[1],
    };
    eprintln!(
        "D1 bench: raw {} B load {:.2} / {:.2} ms; deflated {} B load {:.2} / {:.2} ms (median / p90) → {:?}",
        b.raw.0,
        b.raw.1,
        b.raw.2,
        b.deflate.0,
        b.deflate.1,
        b.deflate.2,
        b.choice()
    );
    Ok(b)
}
