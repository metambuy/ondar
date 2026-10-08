//! M4c S3: `gather` and the per-dot `locate` timed on the four geo slices, release build. Per
//! country, 10 warm-ups + 100 runs of each stage, median / p90 ms: `locator` (the country's rings
//! decoded once), `gather` (Q4), `locate` (every group against the locator) and `all`
//! (`Store::gather_dots`, the three together, as `map_select`'s task will run it). S3's budget:
//! each under 10 ms p90. Then the counts, and the fit frame's dot stats at 300 and 178.
//!
//! cargo run -p ondar-map --example dots_bench --release [-- PATH]
//! PATH defaults to src-tauri/resources/map/world.ondarmap. The slices are
//! `crates/ondar-stations/fixtures/stations-{PT,US,BR,RU}-geo.json`, read as the normaliser reads
//! them (`stationuuid`, `geo_lat` / `geo_long` with (0, 0) as none, `state` trimmed).

use ondar_map::format::Store;
use ondar_map::frame::Lookup;
use ondar_map::gather::{GATHER_KM, Point, gather};
use ondar_map::rules::Pane;
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

fn points(cc: &str) -> Vec<Point> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../ondar-stations/fixtures/stations-{cc}-geo.json"));
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&p).expect("slice")).expect("json");
    rows.iter()
        .map(|r| {
            let (lat, lon) = (r["geo_lat"].as_f64(), r["geo_long"].as_f64());
            Point {
                id: r["stationuuid"].as_str().unwrap_or("").to_string(),
                geo: match (lat, lon) {
                    (Some(la), Some(lo)) if !(la == 0.0 && lo == 0.0) => Some((la, lo)),
                    _ => None,
                },
                place: r["state"].as_str().unwrap_or("").trim().to_string(),
            }
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/map/world.ondarmap"
        )
        .to_string()
    });
    let s = Store::load(&std::fs::read(&path).expect("resource")).expect("load");
    let (clock, _) = stats(Instant::now);
    println!("resource {path}; clock overhead {:.6} ms", clock);
    println!("stage md / p90 ms, 100 runs after 10 warm-ups");
    for cc in ["PT", "US", "BR", "RU"] {
        let Lookup::Country(c) = s.lookup(cc) else {
            panic!("{cc}")
        };
        let pts = points(cc);
        let groups = gather(&pts, GATHER_KM);
        let loc = s.locator(c);
        let t_loc = stats(|| s.locator(c));
        let t_g = stats(|| gather(&pts, GATHER_KM));
        let t_l = stats(|| {
            groups
                .iter()
                .map(|g| loc.locate(g.lat, g.lon).km)
                .sum::<f64>()
        });
        let t_all = stats(|| s.gather_dots(c, &pts));
        let g = s.gather_dots(c, &pts);
        println!(
            "{cc} locator {:.3} / {:.3}  gather {:.3} / {:.3}  locate {:.3} / {:.3}  all {:.3} / {:.3}",
            t_loc.0, t_loc.1, t_g.0, t_g.1, t_l.0, t_l.1, t_all.0, t_all.1
        );
        println!(
            "{cc} stations {} groups {} dots {} located {} outside {} (in {} groups)",
            g.total,
            groups.len(),
            g.dots.len(),
            g.located,
            g.outside,
            groups.len() - g.dots.len()
        );
        for h in [300, 178] {
            let pane = Pane::band(h);
            let fit = s.fit(c, &pane).expect("fit");
            let f = s.frame_dots(c, &pane, fit, &g).expect("frame");
            println!(
                "{cc} fit at {h}: drawn {} hidden {} (stats {:?})",
                f.dots.len(),
                f.stats.dots_hidden,
                (
                    f.stats.dots_outside,
                    f.stats.stations_located,
                    f.stats.stations_total
                )
            );
        }
    }
}
