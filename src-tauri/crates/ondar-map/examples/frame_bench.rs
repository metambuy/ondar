//! M4a § 7: the load and the frame timings, release build. 10 warm-ups + 100 runs per case,
//! median / p90, the clock's overhead read first; each case run again in reverse order.
//!
//! cargo run -p ondar-map --example frame_bench --release [-- PATH]
//! PATH defaults to src-tauri/resources/map/world.ondarmap (the bundle's copy can be given).

use ondar_map::format::Store;
use ondar_map::frame::{Lookup, View};
use ondar_map::rules::{FLOOR_KM_PER_PT, Pane};
use std::time::Instant;

fn stats<T>(mut f: impl FnMut() -> T) -> (f64, f64) {
    for _ in 0..10 {
        std::hint::black_box(f());
    }
    let mut v: Vec<f64> = (0..100)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    v.sort_by(f64::total_cmp);
    ((v[49] + v[50]) / 2.0, v[89])
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}/../../resources/map/world.ondarmap",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let clock = {
        let t = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(Instant::now());
        }
        t.elapsed().as_secs_f64() * 1e9 / 1000.0
    };
    println!("clock overhead {clock:.0} ns per read; resource {path}");
    let (lm, lp) = stats(|| {
        Store::load(&std::fs::read(&path).unwrap())
            .unwrap()
            .raw_bytes()
    });
    let bytes = std::fs::read(&path).unwrap();
    println!(
        "load (read + parse + inflate + CRC)\t{} B\t{lm:.3}\t{lp:.3} ms",
        bytes.len()
    );
    let s = Store::load(&bytes).unwrap();
    let pane = Pane::GOLDEN;
    let idx = |code: &str| match s.lookup(code) {
        Lookup::Country(c) => c,
        Lookup::NoMap => panic!("{code}"),
    };
    let city_view = |c: usize, lon: f64, lat: f64, scale: f64| {
        let ct = &s.countries[c];
        let [x, y] = ondar_map::laea::Laea::new(ct.lat0, ct.lon0)
            .fwd(lon, lat)
            .unwrap();
        View {
            centre: [x, y],
            scale,
        }
    };
    let mut cases: Vec<(String, usize, View)> = Vec::new();
    for (code, city, lon, lat) in [
        ("PT", "Lisbon", -9.1393, 38.7223),
        ("US", "New York", -74.0060, 40.7128),
        ("RU", "Vladivostok", 131.8855, 43.1155),
    ] {
        let c = idx(code);
        let fit = s.fit(c, &pane).unwrap();
        cases.push((format!("{code} fit {:.3}", fit.scale), c, fit));
        cases.push((
            format!("{code} {city} floor"),
            c,
            city_view(c, lon, lat, FLOOR_KM_PER_PT),
        ));
        let mid = (fit.scale * FLOOR_KM_PER_PT).sqrt();
        if mid > FLOOR_KM_PER_PT + 1e-9 {
            cases.push((
                format!("{code} {city} mid {mid:.2}"),
                c,
                city_view(c, lon, lat, mid),
            ));
        }
    }
    println!(
        "case\tmedian ms\tp90 ms\treverse median\tvertices\tbytes_out\tclamped scale\tmissing blobs"
    );
    let mut fwd = Vec::new();
    for (name, c, v) in &cases {
        fwd.push(stats(|| s.frame(*c, &pane, *v).unwrap()));
        let _ = name;
    }
    let mut rev = vec![(0.0, 0.0); cases.len()];
    for (i, (_, c, v)) in cases.iter().enumerate().rev() {
        rev[i] = stats(|| s.frame(*c, &pane, *v).unwrap());
    }
    for (i, (name, c, v)) in cases.iter().enumerate() {
        let f = s.frame(*c, &pane, *v).unwrap();
        let json = serde_json::to_vec(&f).unwrap().len();
        println!(
            "{name}\t{:.3}\t{:.3}\t{:.3}\t{}\t{json}\t{:.3}\t{}",
            fwd[i].0, fwd[i].1, rev[i].0, f.stats.vertices, f.view.scale, f.stats.missing_blobs
        );
    }
    // the sweep: the fit of every country, timed once each after a warm-up
    let mut t: Vec<(f64, String)> = (0..s.countries.len())
        .map(|c| {
            let fit = s.fit(c, &pane).unwrap();
            std::hint::black_box(s.frame(c, &pane, fit));
            let t0 = Instant::now();
            std::hint::black_box(s.frame(c, &pane, fit));
            (
                t0.elapsed().as_secs_f64() * 1e3,
                String::from_utf8_lossy(&s.countries[c].code).into(),
            )
        })
        .collect();
    t.sort_by(|a, b| a.0.total_cmp(&b.0));
    let p90 = t[(t.len() * 9) / 10].0;
    let max = t.last().unwrap();
    println!(
        "sweep: fit of all {} codes — median {:.3} ms, p90 {p90:.3} ms, max {:.3} ms ({})",
        t.len(),
        t[t.len() / 2].0,
        max.0,
        max.1
    );
    let missing: usize = (0..s.countries.len())
        .map(|c| {
            s.frame(c, &pane, s.fit(c, &pane).unwrap())
                .unwrap()
                .stats
                .missing_blobs
        })
        .sum();
    println!("missing blobs over the 248 fits: {missing}");
}
