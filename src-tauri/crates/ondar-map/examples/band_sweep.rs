//! M4b's acceptance sweep (plan § 4, § 9; Step 0's `pane_sweep` promoted): every integer band
//! height from `BAND_FLOOR` to `BAND_MAX` × every country on the shipped resource, through the
//! frame itself — `missing_blobs` at the fit view, `insets_dropped`, every drawn inset's clearance
//! and its box against the controls' rect and the 36 × 28 pt minimum; with `--views`, also
//! `missing_blobs` over D6's extreme views (each level's finest and coarsest scale, the fit
//! rectangle's four corners and centre), which is the slow half.
//!
//! cargo run -p ondar-map --example band_sweep --release [-- [--views] [PATH]]
//!
//! Output: a summary, then one `band` line per height (`h  missing_fit  missing_views  views
//! dropped  under_12  under_min  on_controls`) and one `inset` line per inset per height where
//! it is dropped, under 12 pt, under the minimum or on the controls.

use ondar_map::format::Store;
use ondar_map::frame::View;
use ondar_map::rules::{self, INSET_CLEARANCE_PT, LADDER};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let views = args.iter().any(|a| a == "--views");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| {
            format!(
                "{}/../../resources/map/world.ondarmap",
                env!("CARGO_MANIFEST_DIR")
            )
        });
    let bytes = std::fs::read(&path).expect("resource");
    let s = Store::load(&bytes).expect("load");
    let bands = s.header.bands;
    println!(
        "# band_sweep: {} B, {} countries, bands {}..={} at {} wide, views {}",
        bytes.len(),
        s.countries.len(),
        bands.h_min,
        bands.h_max,
        bands.width,
        views
    );
    let (
        mut t_missing_fit,
        mut t_missing_views,
        mut t_dropped,
        mut t_under12,
        mut t_under_min,
        mut t_controls,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut dropped_by: std::collections::BTreeMap<String, Vec<u32>> = Default::default();
    let [min_w, min_h] = rules::INSET_MIN_LAND_PT;
    let (min_bw, min_bh) = (
        min_w + 2.0 * rules::INSET_PAD_PT,
        min_h + rules::INSET_LABEL_PT + 2.0 * rules::INSET_PAD_PT,
    );
    for h in bands.h_min..=bands.h_max {
        let pane = bands.pane(h);
        let controls = rules::controls_rect(&pane);
        let (uw, uh) = pane.usable();
        let (
            mut missing_fit,
            mut missing_views,
            mut n_views,
            mut dropped,
            mut under12,
            mut under_min,
            mut on_controls,
        ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        for (ci, c) in s.countries.iter().enumerate() {
            let code = String::from_utf8_lossy(&c.code).to_string();
            let fit = s.fit(ci, &pane).expect("fit");
            let f = s.frame(ci, &pane, fit).expect("frame");
            missing_fit += f.stats.missing_blobs;
            dropped += f.stats.insets_dropped;
            if f.stats.insets_dropped > 0 {
                for ins in &c.insets {
                    if !f.insets.iter().any(|d| d.label == ins.label) {
                        dropped_by
                            .entry(format!("{code} {}", ins.label))
                            .or_default()
                            .push(h);
                        println!("inset\t{h}\t{code}\t{}\tdropped", ins.label);
                    }
                }
            }
            for (label, d) in s.inset_clearance(ci, &pane) {
                if d < INSET_CLEARANCE_PT {
                    under12 += 1;
                    println!("inset\t{h}\t{code}\t{label}\tclearance {d:.2}");
                }
            }
            for ins in &f.insets {
                let r = ins.rect.map(f64::from);
                if r[2] < min_bw - 1e-6 || r[3] < min_bh - 1e-6 {
                    under_min += 1;
                    println!(
                        "inset\t{h}\t{code}\t{}\tbox {:.1} × {:.1}",
                        ins.label, r[2], r[3]
                    );
                }
                if !rules::boxes_apart(r, controls) {
                    on_controls += 1;
                    println!("inset\t{h}\t{code}\t{}\ton the controls", ins.label);
                }
            }
            if views {
                // D6's extreme views, as `coverage_matches_clamp` enumerates them
                let [x0, y0, x1, y1] = c.bbox_km;
                let fit_s = ((x1 - x0) / uw).max((y1 - y0) / uh);
                let top = rules::initial_scale(fit_s);
                let (bcx, bcy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                let (fw, fh) = (pane.width / 2.0 * top, pane.height / 2.0 * top);
                let [fx0, fy0, fx1, fy1] = [bcx - fw, bcy - fh, bcx + fw, bcy + fh];
                for (k, &level) in LADDER.iter().enumerate().take(rules::level_for(top) + 1) {
                    let finest = level.max(rules::FLOOR_KM_PER_PT).min(top);
                    let coarsest = LADDER
                        .get(k + 1)
                        .map_or(top, |n| (n * (1.0 - 1e-9)).min(top));
                    for scale in [finest, coarsest] {
                        let (hw, hh) = (pane.width / 2.0 * scale, pane.height / 2.0 * scale);
                        let (cx0, cx1) = (fx0 + hw, fx1 - hw);
                        let (cy0, cy1) = (fy0 + hh, fy1 - hh);
                        for (cx, cy) in [
                            (cx0, cy0),
                            (cx1, cy0),
                            (cx0, cy1),
                            (cx1, cy1),
                            ((cx0 + cx1) / 2.0, (cy0 + cy1) / 2.0),
                        ] {
                            n_views += 1;
                            let v = View {
                                centre: [cx, cy],
                                scale,
                            };
                            missing_views +=
                                s.frame(ci, &pane, v).expect("frame").stats.missing_blobs;
                        }
                    }
                }
            }
        }
        println!(
            "band\t{h}\t{missing_fit}\t{missing_views}\t{n_views}\t{dropped}\t{under12}\t{under_min}\t{on_controls}"
        );
        t_missing_fit += missing_fit;
        t_missing_views += missing_views;
        t_dropped += dropped;
        t_under12 += under12;
        t_under_min += under_min;
        t_controls += on_controls;
    }
    println!(
        "# totals over {} bands: missing_blobs at fit {t_missing_fit}, over D6's views {t_missing_views}; insets dropped {t_dropped}; drawn under 12 pt {t_under12}; under the 36 × 28 minimum {t_under_min}; on the controls {t_controls}",
        bands.h_max - bands.h_min + 1
    );
    for (k, hs) in &dropped_by {
        let (lo, hi) = (hs.iter().min().unwrap(), hs.iter().max().unwrap());
        println!("# dropped: {k} at {} band(s), {lo}–{hi}", hs.len());
    }
}
