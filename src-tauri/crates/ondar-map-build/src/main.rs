//! `ondar-map-build`: Natural Earth 10m v5.1.2 → `world.ondarmap` (M4a). A build-time tool, never
//! part of the app; its inputs are fetched by `scripts/fetch-natural-earth.sh` and pinned.
//!
//! ```text
//! cargo run -p ondar-map-build --release -- [--input DIR] --tables DIR
//! ```

mod borders;
mod geom;
mod ne;
mod pins;
mod seam;
mod tables;
mod world;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use world::{CountryPlan, GroupRole, World};

pub const NE_TAG: &str = "v5.1.2";

struct Args {
    input: PathBuf,
    tables: Option<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        input: Path::new(env!("CARGO_MANIFEST_DIR")).join("input"),
        tables: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = || {
            it.next()
                .map(PathBuf::from)
                .ok_or(format!("{x} needs a value"))
        };
        match x.as_str() {
            "--input" => a.input = val()?,
            "--tables" => a.tables = Some(val()?),
            other => return Err(format!("unknown argument {other}")),
        }
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
        "code\tlabel\tcorner\tx\ty\tw\th\tgroup_parts\tgroup_area_km2\tanchor_km\tlat0\tlon0\tscale_km_per_pt\tlevel\tclearance_pt\n",
    );
    for p in &inp.plans {
        for i in &p.insets {
            let g = &p.groups[i.group];
            let [x, y, w, h] = i.row.rect;
            let _ = writeln!(
                t,
                "{}\t{}\t{:?}\t{x}\t{y}\t{w}\t{h}\t{}\t{:.0}\t{:.2}\t{:.6}\t{:.6}\t{:.4}\t{}\t{:.2}",
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
                i.clearance_pt
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
    if let Some(dir) = a.tables {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, body) in [
            ("fit.tsv", fit_table(&inp)),
            ("insets.tsv", inset_table(&inp)),
            ("s4.tsv", s4_table(&inp)),
            ("borders.tsv", borders_table(&borders)),
        ] {
            std::fs::write(dir.join(name), body).map_err(|e| format!("{name}: {e}"))?;
        }
        eprintln!("tables written to {}", dir.display());
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("ondar-map-build: {e}");
        std::process::exit(1);
    }
}
