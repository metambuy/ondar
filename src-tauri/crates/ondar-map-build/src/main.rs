//! `ondar-map-build`: Natural Earth 10m v5.1.2 → `world.ondarmap` (M4a). A build-time tool, never
//! part of the app; its inputs are fetched by `scripts/fetch-natural-earth.sh` and pinned.
//!
//! ```text
//! cargo run -p ondar-map-build --release -- [--input DIR] [--tables DIR]
//!     [--out FILE] [--report FILE] [--encoding deflate|raw] [--bench]
//! ```

mod bench;
mod borders;
mod geom;
mod ne;
mod pins;
mod report;
mod seam;
mod simplify;
mod store;
mod tables;
mod world;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use world::{CountryPlan, GroupRole, World};

pub const NE_TAG: &str = "v5.1.2";

struct Args {
    input: PathBuf,
    tables: Option<PathBuf>,
    out: Option<PathBuf>,
    report: Option<PathBuf>,
    encoding: ondar_map::format::Encoding,
    simplifier: store::Simplifier,
    bench: bool,
    /// M4b commit 4: build although a label is wider than its box (review P3) or an inset is
    /// dropped at 178 or 300 (the brief's STOP) — for the tables at the STOP, never for a shipped
    /// resource: `parse` refuses either with `--out`.
    allow_wide_labels: bool,
    allow_dropped_insets: bool,
}

fn args() -> Result<Args, String> {
    parse(std::env::args().skip(1))
}

/// The arguments after the program's name.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        input: Path::new(env!("CARGO_MANIFEST_DIR")).join("input"),
        tables: None,
        out: None,
        report: None,
        encoding: ondar_map::format::Encoding::Deflate,
        simplifier: store::Simplifier::Hybrid,
        bench: false,
        allow_wide_labels: false,
        allow_dropped_insets: false,
    };
    while let Some(x) = it.next() {
        let mut val = || {
            it.next()
                .map(PathBuf::from)
                .ok_or(format!("{x} needs a value"))
        };
        match x.as_str() {
            "--input" => a.input = val()?,
            "--tables" => a.tables = Some(val()?),
            "--out" => a.out = Some(val()?),
            "--report" => a.report = Some(val()?),
            "--bench" => a.bench = true,
            "--allow-wide-labels" => a.allow_wide_labels = true,
            "--allow-dropped-insets" => a.allow_dropped_insets = true,
            "--simplifier" => {
                a.simplifier = match it.next().as_deref() {
                    Some("hybrid") => store::Simplifier::Hybrid,
                    Some("vw") => store::Simplifier::Vw,
                    other => return Err(format!("--simplifier hybrid|vw, not {other:?}")),
                }
            }
            "--encoding" => {
                a.encoding = match it.next().as_deref() {
                    Some("deflate") => ondar_map::format::Encoding::Deflate,
                    Some("raw") => ondar_map::format::Encoding::Raw,
                    other => return Err(format!("--encoding deflate|raw, not {other:?}")),
                }
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    // checked here, not at the gate: `main` writes the tables before the gate runs
    if a.out.is_some() && (a.allow_dropped_insets || a.allow_wide_labels) {
        return Err(
            "--allow-dropped-insets / --allow-wide-labels are for the tables at a STOP, never with --out"
                .into(),
        );
    }
    Ok(a)
}

pub struct Inputs {
    pub world: World,
    pub admin1: Vec<ne::Admin1>,
    pub plans: Vec<CountryPlan>,
    pub aliases: Vec<tables::Alias>,
}

pub fn load(input: &Path) -> Result<Inputs, String> {
    let files = pins::check_all(input)?;
    let get = |n: &str| {
        files
            .get(n)
            .map(Vec::as_slice)
            .ok_or(format!("{n} missing"))
    };
    let layer = |l: &str| -> Result<ne::Layer<'_>, String> {
        Ok(ne::Layer {
            shp: get(&format!("ne_10m_{l}.shp"))?,
            shx: get(&format!("ne_10m_{l}.shx"))?,
            dbf: get(&format!("ne_10m_{l}.dbf"))?,
        })
    };
    let aliases = tables::aliases(tables::ALIASES_TSV)?;
    let wanted: Vec<&str> = aliases.iter().map(|a| a.gu_a3.as_str()).collect();
    let admin0 = ne::admin0(layer("admin_0_countries")?)?;
    let map_units = ne::map_units(layer("admin_0_map_units")?, &wanted)?;
    let admin1 = ne::admin1(layer("admin_1_states_provinces")?)?;
    let world = World::new(admin0, map_units, &aliases)?;
    if world.seam_edges_left > 0 {
        return Err(format!(
            "R9: {} seam edges left after stitching",
            world.seam_edges_left
        ));
    }
    let plans = world::plan_all(
        &world,
        &admin1,
        &tables::overrides(tables::OVERRIDES_TSV)?,
        &tables::insets(tables::INSETS_TSV)?,
        &aliases,
    )?;
    Ok(Inputs {
        world,
        admin1,
        plans,
        aliases,
    })
}

fn fit_table(inp: &Inputs) -> String {
    let mut t = String::from(
        "code\ta3\tname\tlat0\tlon0\tmin_x_km\tmin_y_km\tmax_x_km\tmax_y_km\tfit_km_per_pt\tinitial_km_per_pt\tlevel\tsubdivisions\toverride\talias\tinsets\tparts\tgroups\tframe_parts\tdropped_groups\n",
    );
    for p in &inp.plans {
        let frame_parts: usize = p
            .groups
            .iter()
            .filter(|g| g.role == GroupRole::Frame)
            .map(|g| g.parts.len())
            .sum();
        let dropped = p
            .groups
            .iter()
            .filter(|g| g.role == GroupRole::Dropped)
            .count();
        let _ = writeln!(
            t,
            "{}\t{}\t{}\t{:.10}\t{:.10}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            p.code,
            inp.world.units[p.units[0]].a3,
            p.name,
            p.lat0,
            p.lon0,
            p.bbox[0],
            p.bbox[1],
            p.bbox[2],
            p.bbox[3],
            p.fit,
            p.initial(),
            p.level(),
            u8::from(p.subdivisions),
            u8::from(p.overridden),
            u8::from(p.alias),
            p.insets.len(),
            p.parts.len(),
            p.groups.len(),
            frame_parts,
            dropped
        );
    }
    t
}

fn inset_table(inp: &Inputs) -> String {
    let mut t = String::from(
        "code\tlabel\tcorner\tx\ty\tw\th\tgroup_parts\tgroup_area_km2\tanchor_km\tlat0\tlon0\tscale_km_per_pt\tlevel\tclearance_pt\tpct_178\tpct_300\tmin_pct\tmin_at\tfirst_band\n",
    );
    for p in &inp.plans {
        for i in &p.insets {
            let g = &p.groups[i.group];
            let [x, y, w, h] = i.row.rect;
            let at = |h: u32| {
                ondar_map::rules::band_index(h)
                    .and_then(|k| i.scale_pct.get(k))
                    .copied()
                    .unwrap_or(0)
            };
            let (min_i, &min_pct) = i
                .scale_pct
                .iter()
                .enumerate()
                .min_by_key(|&(_, &p)| p)
                .unwrap_or((0, &0));
            let _ = writeln!(
                t,
                "{}\t{}\t{:?}\t{x}\t{y}\t{w}\t{h}\t{}\t{:.0}\t{:.2}\t{:.6}\t{:.6}\t{:.4}\t{}\t{:.2}\t{}\t{}\t{min_pct}\t{}\t{}",
                p.code,
                i.row.label,
                i.row.corner,
                g.parts.len(),
                g.area_km2,
                i.anchor_km,
                i.lat0,
                i.lon0,
                i.scale,
                ondar_map::rules::level_for(i.scale),
                i.clearance_pt,
                at(178),
                at(300),
                ondar_map::rules::BAND_FLOOR + min_i as u32,
                world::first_band(i).map_or("never".to_string(), |h| h.to_string())
            );
        }
    }
    t
}

/// I1 (M4b commit 4): per inset, the scale in percent at every band height, one row per height.
fn inset_bands_table(inp: &Inputs) -> String {
    let mut t = String::from("code\tlabel\tcorner\th\tscale_pct\tx\ty\tw\th_pt\tinner_width\n");
    for p in &inp.plans {
        for i in &p.insets {
            for (k, &pct) in i.scale_pct.iter().enumerate() {
                let h = ondar_map::rules::BAND_FLOOR + k as u32;
                // the placed rect (the stacking rule applied); zero where dropped
                let r = i.rects.get(k).copied().unwrap_or([0.0; 4]);
                let _ = writeln!(
                    t,
                    "{}\t{}\t{:?}\t{h}\t{pct}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
                    p.code,
                    i.row.label,
                    i.row.corner,
                    r[0],
                    r[1],
                    r[2],
                    r[3],
                    ondar_map::rules::label_inner_width(r)
                );
            }
        }
    }
    t
}

/// The corner table (M4b commit 4): per inset and corner in {TL, TR, BL}, the box alone after the
/// controls — the minimum scale over the bands and where, the scales at 178 and 300, the full
/// box's clearance at 161 (Step 0's figure).
fn inset_corners_table(inp: &Inputs) -> String {
    let mut t = String::from(
        "code\tlabel\tcurrent\tcorner\tx\ty\tw\th\tmin_pct\tmin_at\tpct_178\tpct_300\tclearance_161\n",
    );
    for p in &inp.plans {
        for i in &p.insets {
            for c in &i.corners {
                let _ = writeln!(
                    t,
                    "{}\t{}\t{:?}\t{:?}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}\t{:.2}",
                    p.code,
                    i.row.label,
                    i.row.corner,
                    c.corner,
                    c.rect[0],
                    c.rect[1],
                    c.rect[2],
                    c.rect[3],
                    c.min_pct,
                    c.min_at,
                    c.pct_178,
                    c.pct_300,
                    c.clearance_161
                );
            }
        }
    }
    t
}

/// Review P3 (M4b commit 4): per inset, the label's conservative width at the artifact's 8 pt
/// against the box's inner width at 178 and 300 (at the band's scale).
fn inset_labels_table(inp: &Inputs) -> String {
    let mut t = String::from(
        "code\tlabel\tchars\twidth_pt\tpct_178\tinner_178\tfits_178\tpct_300\tinner_300\tfits_300\n",
    );
    for p in &inp.plans {
        for i in &p.insets {
            let width = ondar_map::rules::label_width_pt(&i.row.label);
            let at = |h: u32| {
                let pct = ondar_map::rules::band_index(h)
                    .and_then(|k| i.scale_pct.get(k))
                    .copied()
                    .unwrap_or(0);
                let inner = if pct == 0 {
                    0.0
                } else {
                    ondar_map::rules::label_inner_width(ondar_map::rules::inset_box_at(
                        i.row.rect,
                        i.row.corner,
                        &ondar_map::rules::Pane::band(h),
                        f64::from(pct) / 100.0,
                    ))
                };
                (pct, inner, pct > 0 && width <= inner)
            };
            let (p178, i178, f178) = at(178);
            let (p300, i300, f300) = at(300);
            let _ = writeln!(
                t,
                "{}\t{}\t{}\t{width:.1}\t{p178}\t{i178:.1}\t{}\t{p300}\t{i300:.1}\t{}",
                p.code,
                i.row.label,
                i.row.label.chars().count(),
                u8::from(f178),
                u8::from(f300)
            );
        }
    }
    t
}

fn s4_table(inp: &Inputs) -> String {
    let mut t = String::from("code\tgu_a3\tparent_a3\tparts\tparent_parts_matched\n");
    for ((code, hits, n), a) in world::s4_matches(&inp.world, &inp.aliases)
        .iter()
        .zip(&inp.aliases)
    {
        let _ = writeln!(
            t,
            "{code}\t{}\t{}\t{n}\t{}",
            a.gu_a3,
            a.parent_a3,
            hits.len()
        );
    }
    t
}

fn borders_table(b: &[borders::CountryBorders]) -> String {
    let mut t = String::from(
        "code\tadmin1\tedges\tonce\ttwice\tthrice_or_more\tseam\tlines\tgate_far_edges\tgate_worst_m\tfar_inside_land\tfar_inside_worst_m\tgate\n",
    );
    for x in b {
        let c = &x.census;
        let _ = writeln!(
            t,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{:.3}\t{}",
            x.code,
            x.admin1,
            c.edges_total,
            c.once.len(),
            c.twice,
            c.thrice_or_more,
            c.seam,
            c.lines.len(),
            x.gate_far,
            x.gate_worst_km * 1000.0,
            x.far_inside,
            x.far_inside_worst_km * 1000.0,
            if x.passes() { "PASS" } else { "FAIL" }
        );
    }
    t
}

/// The tool's commit (40 hex) and whether the tree had changes.
pub fn git_head() -> (String, bool) {
    let dir = env!("CARGO_MANIFEST_DIR");
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let head = run(&["rev-parse", "HEAD"])
        .filter(|h| h.len() == 40)
        .unwrap_or_else(|| "0".repeat(40));
    let dirty = run(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    (head, dirty)
}

pub fn header_pins() -> Result<ondar_map::format::Pins, String> {
    Ok(ondar_map::format::Pins {
        tool_git: git_head().0,
        ne_tag: NE_TAG.to_string(),
        inputs: pins::parse(pins::PINS_TSV)?
            .into_iter()
            .map(|p| (p.file, p.sha256))
            .collect(),
    })
}

fn run() -> Result<(), String> {
    let a = args()?;
    let t0 = std::time::Instant::now();
    let inp = load(&a.input)?;
    eprintln!(
        "loaded and planned in {:.1} s: {} units ({} vertices), {} countries; stitched {}",
        t0.elapsed().as_secs_f64(),
        inp.world.units.len(),
        inp.world
            .units
            .iter()
            .map(|u| geom::vertices(&u.parts))
            .sum::<usize>(),
        inp.plans.len(),
        inp.world
            .stitched
            .iter()
            .map(|(a3, s)| format!(
                "{a3} (seam parts {}, parts {} → {}, removed {} vertices, {} notches ≤ {:.3} km)",
                s.seam_parts,
                s.parts_in,
                s.parts_out,
                s.removed_vertices,
                s.notches,
                s.notch_max_km
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let t1 = std::time::Instant::now();
    let borders: Vec<borders::CountryBorders> = inp
        .plans
        .iter()
        .filter(|p| p.subdivisions)
        .map(|p| borders::for_country(p, &inp.world, &inp.admin1))
        .collect();
    let pass = borders.iter().filter(|b| b.passes()).count();
    eprintln!(
        "D2 edge census for the {} subdivision countries in {:.1} s: the gate passes for {pass}",
        borders.len(),
        t1.elapsed().as_secs_f64()
    );
    // the tables first (M4b commit 4: the STOP reads them even when the gate below refuses)
    if let Some(dir) = &a.tables {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, body) in [
            ("fit.tsv", fit_table(&inp)),
            ("insets.tsv", inset_table(&inp)),
            ("inset-bands.tsv", inset_bands_table(&inp)),
            ("inset-corners.tsv", inset_corners_table(&inp)),
            ("inset-labels.tsv", inset_labels_table(&inp)),
            ("s4.tsv", s4_table(&inp)),
            ("borders.tsv", borders_table(&borders)),
        ] {
            std::fs::write(dir.join(name), body).map_err(|e| format!("{name}: {e}"))?;
        }
        eprintln!("tables written to {}", dir.display());
    }
    // the ship gate (M4b commit 4): no inset dropped at 178 or 300, no label wider than its box
    let gate = world::ship_gate(&inp.plans);
    for d in &gate.dropped {
        eprintln!("inset dropped: {d}");
    }
    for w in &gate.wide_labels {
        eprintln!("label too wide: {w}");
    }
    if !gate.dropped.is_empty() && !a.allow_dropped_insets {
        return Err(format!(
            "{} inset(s) dropped at 328 × 178 or × 300 (the brief's STOP); --allow-dropped-insets builds regardless, for the tables only",
            gate.dropped.len()
        ));
    }
    if !gate.wide_labels.is_empty() && !a.allow_wide_labels {
        return Err(format!(
            "{} label(s) wider than their box (review P3); shorten them in insets.tsv, or --allow-wide-labels for the tables only",
            gate.wide_labels.len()
        ));
    }
    if a.out.is_some() || a.report.is_some() || a.bench {
        let built = store::build(
            &inp.world,
            &inp.admin1,
            &inp.plans,
            &inp.aliases,
            header_pins()?,
            a.simplifier,
        )?;
        eprintln!(
            "store built in {:.1} s: {} blobs; P4 RU land at 24 km/pt: {} in, {} per-ring VW, {} RDP, {} stored ({:?})",
            built.seconds,
            built.blobs.len(),
            built.p4.0,
            built.p4.1,
            built.p4.2,
            built.p4.3,
            built.simplifier
        );
        eprintln!(
            "coverage for bands {}..={}: the bound adds {} blob(s), {} B of {} B deflated{}; {} collapsed ring(s)",
            built.bands.0,
            built.bands.1,
            built.bound_added.blobs,
            built.bound_added.bytes,
            built.bound_added.total_bytes,
            if built.bound_added.exact_stored {
                " — over 5 %, the exact union stored"
            } else {
                ""
            },
            built.collapsed.len()
        );
        let bench = if a.bench {
            Some(bench::run(&built)?)
        } else {
            None
        };
        let bytes = built.write(a.encoding).map_err(|e| format!("write: {e}"))?;
        if let Some(out) = &a.out {
            std::fs::write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
            eprintln!(
                "wrote {} ({} B, {:?})",
                out.display(),
                bytes.len(),
                a.encoding
            );
        }
        if let Some(path) = &a.report {
            let r = report::write(
                &inp,
                &built,
                &bytes,
                a.encoding,
                bench.as_ref(),
                &borders,
                t0.elapsed().as_secs_f64(),
            );
            std::fs::write(path, r).map_err(|e| format!("{}: {e}", path.display()))?;
            eprintln!("report written to {}", path.display());
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("ondar-map-build: {e}");
        std::process::exit(1);
    }
}

/// The input-bound checks (`#[ignore]`: the NE inputs never enter CI). Run at each build:
/// `cargo test -p ondar-map-build --release -- --ignored`, the result in the commit message.
#[cfg(test)]
mod args_tests {
    use super::*;

    fn parse_strs(a: &[&str]) -> Result<Args, String> {
        parse(a.iter().map(|s| s.to_string()))
    }

    /// An `--allow-*` flag builds past the ship gate, for the tables at a STOP only: with `--out`
    /// it is refused in the parser, before `main` writes the tables (M4b's review, latent 10).
    /// Each flag alone, or `--out` alone, parses. Fails if either refusal is dropped.
    #[test]
    fn an_allow_flag_with_out_is_refused() {
        for flag in ["--allow-dropped-insets", "--allow-wide-labels"] {
            assert!(
                parse_strs(&[flag, "--out", "w.ondarmap"]).is_err(),
                "{flag} then --out"
            );
            assert!(
                parse_strs(&["--out", "w.ondarmap", flag]).is_err(),
                "--out then {flag}"
            );
            assert!(
                parse_strs(&[flag, "--tables", "t"]).is_ok(),
                "{flag} with --tables"
            );
        }
        assert!(parse_strs(&["--out", "w.ondarmap"]).is_ok());
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use std::sync::OnceLock;

    fn inputs() -> &'static Inputs {
        static I: OnceLock<Inputs> = OnceLock::new();
        I.get_or_init(|| load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("input")).unwrap())
    }

    /// Step 0 reproduced: the 239 codes' fits equal Q1-merged's within 0.05 % or the fixture's
    /// rounding (half a unit in its 4th decimal), except the three listed differences: MY's
    /// override (2.326, the peninsula), AQ pole-centred, MM's subdivisions on at 8.008. Fails
    /// with AQ's pole centre removed.
    #[test]
    #[ignore]
    fn step0_reproduced() {
        let fx = include_str!("../../ondar-map/fixtures/step0-fit.tsv");
        let mut n = 0;
        for line in fx.lines().skip(1) {
            let (code, fit) = line.split_once('\t').unwrap();
            let step0: f64 = fit.parse().unwrap();
            let p = inputs().plans.iter().find(|p| p.code == code).unwrap();
            match code {
                "MY" => assert!(
                    (p.fit - 2.3257).abs() < 1e-3 && p.overridden,
                    "MY {}",
                    p.fit
                ),
                "AQ" => assert_eq!((p.lat0, p.lon0), (-90.0, 0.0)),
                _ => {
                    let tol = (step0 * 5e-4).max(5e-5);
                    assert!((p.fit - step0).abs() <= tol, "{code}: {} vs {step0}", p.fit);
                    n += 1;
                }
            }
        }
        assert_eq!(n, 237);
        let mm = inputs().plans.iter().find(|p| p.code == "MM").unwrap();
        assert!(
            mm.subdivisions && mm.fit > 8.0 && mm.fit < 8.01,
            "MM {}",
            mm.fit
        );
        assert_eq!(inputs().plans.iter().filter(|p| p.subdivisions).count(), 18);
        assert_eq!(inputs().plans.len(), 248);
        assert_eq!(inputs().world.units.len(), 267);
    }

    /// R9: the stitched units are AQ, RU and FJ, and no seam edge is left anywhere.
    #[test]
    #[ignore]
    fn stitched_units() {
        let mut s: Vec<&str> = inputs()
            .world
            .stitched
            .iter()
            .map(|(a3, _)| a3.as_str())
            .collect();
        s.sort();
        assert_eq!(s, ["ATA", "FJI", "RUS"]);
        assert_eq!(inputs().world.seam_edges_left, 0);
    }

    /// D2's census for the 18: no edge found three times; the gate (once-edges inside the land
    /// within 375 m of admin 0) passes for all 18; the farthest is under 80 m (79 m, US). Fails
    /// with the inside-land filter dropped.
    #[test]
    #[ignore]
    fn the_edge_gate_for_the_18() {
        let i = inputs();
        let b: Vec<borders::CountryBorders> = i
            .plans
            .iter()
            .filter(|p| p.subdivisions)
            .map(|p| borders::for_country(p, &i.world, &i.admin1))
            .collect();
        assert_eq!(b.len(), 18);
        assert!(b.iter().all(|x| x.census.thrice_or_more == 0));
        assert!(b.iter().all(|x| x.passes()));
        let inside = b.iter().map(|x| x.far_inside_worst_km).fold(0.0, f64::max);
        assert!(inside < 0.080, "{inside} km");
    }

    /// Review P6 (M4b commit 4): the tool's corner table at 328 × 161 with the controls' rect
    /// disabled against Step 0's (`fixtures/step0-corners-161.tsv`, the shipped resource framed
    /// by `Store::frame`), for the eight countries Step 0 framed on complete land (IN and US had
    /// 11 and 48 missing blobs there): the full box's clearance within 0.3 pt (the drawn bound,
    /// 0.25 + 0.035, and the table's 0.01 rounding: Step 0 measured simplified rings, the tool
    /// the input), and the largest scale within 0.02 of Step 0's bisected fraction — or 0 where
    /// that fraction is under the row's minimum, which Step 0 did not apply. All 33 agree.
    #[test]
    #[ignore]
    fn corner_table_at_161_matches_step0() {
        let i = inputs();
        let t = include_str!("../fixtures/step0-corners-161.tsv");
        let name = |c: ondar_map::format::Corner| match c {
            ondar_map::format::Corner::TopLeft => "TL",
            ondar_map::format::Corner::TopRight => "TR",
            ondar_map::format::Corner::BottomLeft => "BL",
            ondar_map::format::Corner::BottomRight => "BR",
        };
        let mut compared = 0;
        for p in i
            .plans
            .iter()
            .filter(|p| ["EC", "ES", "FR", "MY", "NO", "PF", "PT", "YE"].contains(&p.code.as_str()))
        {
            let tables = world::inset_tables_for(p, &i.world, false);
            for (ins, corners) in p.insets.iter().zip(&tables.corners) {
                for c in corners {
                    let line = t
                        .lines()
                        .find(|l| {
                            let f: Vec<&str> = l.split('\t').collect();
                            f.first() == Some(&"corner")
                                && f.get(1) == Some(&p.code.as_str())
                                && f.get(2) == Some(&ins.row.label.as_str())
                                && f.get(3) == Some(&name(c.corner))
                        })
                        .unwrap_or_else(|| {
                            panic!("{} {} {}", p.code, ins.row.label, name(c.corner))
                        });
                    let f: Vec<&str> = line.split('\t').collect();
                    let step0_clear: f64 = f[5].parse().unwrap();
                    let step0_frac: f64 = f[7].split(' ').next().unwrap().parse().unwrap();
                    assert!(
                        (c.clearance_161 - step0_clear).abs() <= 0.3,
                        "{} {} {}: clearance {:.2} vs Step 0's {step0_clear}",
                        p.code,
                        ins.row.label,
                        name(c.corner),
                        c.clearance_161
                    );
                    let want = if step0_frac < ondar_map::rules::inset_min_scale(ins.row.rect) {
                        0.0
                    } else {
                        step0_frac
                    };
                    assert!(
                        (f64::from(c.pct_161) / 100.0 - want).abs() <= 0.02,
                        "{} {} {}: {} % vs Step 0's {step0_frac} (minimum {:.3})",
                        p.code,
                        ins.row.label,
                        name(c.corner),
                        c.pct_161,
                        ondar_map::rules::inset_min_scale(ins.row.rect)
                    );
                    compared += 1;
                }
            }
        }
        assert_eq!(
            compared,
            3 * 11,
            "eleven insets in the eight countries, three corners each"
        );
    }

    /// Determinism: two builds give the same bytes from the end of the header on, and every
    /// blob meets its spec.
    #[test]
    #[ignore]
    fn two_builds_are_identical() {
        let i = inputs();
        let build = || {
            store::build(
                &i.world,
                &i.admin1,
                &i.plans,
                &i.aliases,
                header_pins().unwrap(),
                store::Simplifier::Hybrid,
            )
            .unwrap()
        };
        let (a, b) = (build(), build());
        let (wa, wb) = (
            a.write(ondar_map::format::Encoding::Deflate).unwrap(),
            b.write(ondar_map::format::Encoding::Deflate).unwrap(),
        );
        assert_eq!(wa, wb);
        for x in &a.blobs {
            let lim = match x.layer {
                ondar_map::format::Layer::Land => store::LAND_TOL_PT,
                ondar_map::format::Layer::Subdivisions => store::SUB_TOL_PT,
            };
            assert!(
                x.bound_pt <= lim,
                "{:?} {} {}",
                x.layer,
                x.owner,
                x.bound_pt
            );
        }
        let s = ondar_map::format::Store::load(&wa).unwrap();
        assert_eq!(s.blobs.len(), a.blobs.len());
    }
}
