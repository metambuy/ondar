//! The build report, written beside the resource: everything the chat checks at P3 — bytes per
//! layer and level, the measured bounds, R9's stitch, P2, P4, D2, coverage, D1, the build time,
//! the pins and the resource's SHA-256.

use crate::Inputs;
use crate::bench::Bench;
use crate::borders::CountryBorders;
use crate::store::{Built, QUANT_PT, SubStorage};
use ondar_map::format::{Encoding, Layer};
use ondar_map::rules::LADDER;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

fn mb(b: usize) -> String {
    format!("{:.3} MB", b as f64 / 1e6)
}

#[allow(clippy::too_many_arguments)]
pub fn write(
    inp: &Inputs,
    built: &Built,
    bytes: &[u8],
    enc: Encoding,
    bench: Option<&Bench>,
    borders: &[CountryBorders],
    seconds: f64,
) -> String {
    let mut r = String::new();
    let (head, dirty) = crate::git_head();
    let sha: [u8; 32] = Sha256::digest(bytes).into();
    let _ = writeln!(r, "# world.ondarmap — build report\n");
    let _ = writeln!(
        r,
        "Built by `ondar-map-build` at `{head}`{} from Natural Earth 10m {} (the 12 pinned inputs \
         below). Format v{}, {:?} per blob.\n",
        if dirty {
            " (working tree with changes)"
        } else {
            ""
        },
        crate::NE_TAG,
        ondar_map::format::VERSION,
        enc
    );
    let _ = writeln!(r, "| | |\n|---|---|");
    let _ = writeln!(r, "| resource | {} B ({}) |", bytes.len(), mb(bytes.len()));
    let _ = writeln!(r, "| SHA-256 | `{}` |", crate::pins::hex(&sha));
    let _ = writeln!(
        r,
        "| units / countries / blobs | {} / {} / {} |",
        built.units.len(),
        built.countries.len(),
        built.blobs.len()
    );
    let _ = writeln!(
        r,
        "| build time | {seconds:.1} s total (store {:.1} s, {} threads) — the ~10 min budget |",
        built.seconds,
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    let _ = writeln!(r);

    // bytes per layer and level
    let _ = writeln!(r, "## Bytes, vertices and bounds per layer and level\n");
    let _ = writeln!(
        r,
        "Bounds are the exact measure (every original vertex to the simplified line), points at \
         the level; the codec adds at most {QUANT_PT:.4} pt (half a quantum's diagonal). Land's \
         limit is 0.25 pt, subdivisions' 0.5 pt.\n"
    );
    let _ = writeln!(
        r,
        "| layer | level km/pt | blobs | vertices in | vertices out | raw B | deflated B | max bound pt | + codec |"
    );
    let _ = writeln!(r, "|---|---|---|---|---|---|---|---|---|");
    let (mut tot_raw, mut tot_def, mut nb_raw, mut nb_def) = (0usize, 0usize, 0usize, 0usize);
    let mut layer_max = [0f64; 2];
    for layer in [Layer::Land, Layer::Subdivisions] {
        for (k, l) in LADDER.iter().enumerate() {
            let bs: Vec<_> = built
                .blobs
                .iter()
                .filter(|b| b.layer == layer && b.level == k)
                .collect();
            if bs.is_empty() {
                continue;
            }
            let (mut raw, mut def) = (0usize, 0usize);
            for b in &bs {
                let rb = ondar_map::format::blob_raw(&b.rings);
                let d = ondar_map::format::deflate(&rb).map_or(0, |v| v.len());
                raw += rb.len();
                def += d;
                if b.neighbour_only {
                    nb_raw += rb.len();
                    nb_def += d;
                }
            }
            tot_raw += raw;
            tot_def += def;
            let maxb = bs.iter().map(|b| b.bound_pt).fold(0.0, f64::max);
            let li = usize::from(layer == Layer::Subdivisions);
            layer_max[li] = layer_max[li].max(maxb);
            let _ = writeln!(
                r,
                "| {layer:?} | {l} | {} | {} | {} | {raw} | {def} | {maxb:.4} | {:.4} |",
                bs.len(),
                bs.iter().map(|b| b.vertices_in).sum::<usize>(),
                bs.iter().map(|b| b.vertices_out).sum::<usize>(),
                maxb + QUANT_PT
            );
        }
    }
    let _ = writeln!(
        r,
        "\nTotal blob bytes: {tot_raw} B raw ({}), {tot_def} B deflated per blob ({}). \
         Neighbour-only blobs (a unit at a level only other countries' frames need): {nb_raw} B \
         raw, {nb_def} B deflated = {:.1} % of the deflated total. Max bound: land {:.4} pt, \
         subdivisions {:.4} pt (spec 0.25 / 0.5).\n",
        mb(tot_raw),
        mb(tot_def),
        100.0 * nb_def as f64 / tot_def.max(1) as f64,
        layer_max[0],
        layer_max[1]
    );

    // coverage
    let _ = writeln!(r, "## Coverage\n");
    let _ = writeln!(
        r,
        "Reach: **{:?}**. A unit is stored at level k when a ring's cap meets some country's \
         reach at k, or it is in an inset at the inset's level. `Plan` (§ 2 as written): the fit \
         rectangle grown by (W/2 + 2) × the coarsest scale that uses k, capped at the widest \
         view — the view's centre anywhere in the fit rectangle. `Fit`: the view inside the fit \
         rectangle, grown by the 2 pt clip margin only.\n",
        built.reach
    );
    let _ = writeln!(
        r,
        "| level km/pt | units stored (of {}) |\n|---|---|",
        built.units.len()
    );
    for (k, l) in LADDER.iter().enumerate() {
        let n = built
            .blobs
            .iter()
            .filter(|b| b.layer == Layer::Land && b.level == k)
            .count();
        let _ = writeln!(r, "| {l} | {n} |");
    }
    let _ = writeln!(r);

    // P4
    let _ = writeln!(r, "## P4 — per-ring VW against RDP\n");
    let (vin, vw, rdp) = built.p4;
    let _ = writeln!(
        r,
        "RU land at 24 km/pt (open rings, RU's frame LAEA, after the seam stitch): **{vin} in, \
         {vw} per-ring VW, {rdp} RDP** at the same 6 km tolerance — VW keeps {:.2}× RDP. Q2b \
         (one ε per country, closed rings): 36 756 in, 16 667 VW, 4 128 RDP (4.04×).\n",
        vw as f64 / rdp.max(1) as f64
    );
    let _ = writeln!(
        r,
        "| level km/pt | land vertices, per-ring VW | RDP, same tolerance | VW / RDP | RDP rings invalid (of) |\n|---|---|---|---|---|"
    );
    for (k, l) in LADDER.iter().enumerate() {
        let bs: Vec<_> = built
            .blobs
            .iter()
            .filter(|b| b.layer == Layer::Land && b.level == k)
            .collect();
        let vw: usize = bs.iter().map(|b| b.vertices_out).sum();
        let rdp: usize = bs.iter().map(|b| b.rdp_vertices).sum();
        let rings: usize = bs.iter().map(|b| b.rings.len()).sum();
        let bad: usize = bs.iter().map(|b| b.rdp_invalid).sum();
        let _ = writeln!(
            r,
            "| {l} | {vw} | {rdp} | {:.2} | {bad} ({rings}) |",
            vw as f64 / rdp.max(1) as f64
        );
    }
    let _ = writeln!(r);

    // P2
    let _ = writeln!(r, "## P2 — neighbours drawn in another projection\n");
    let _ = writeln!(
        r,
        "Per level, the largest stretch of a displacement between a ring's storage LAEA and a \
         projection that draws it — k'(c_storage) × k'(c_drawing) over the ring's vertices inside \
         that frame's reach (LAEA's scale factors are k' and 1/k'), × level / scale for an inset \
         — and the worst displacement that makes: (ring bound + codec) × stretch, points. A report, not a gate; above ~0.5 pt the remedy \
         (neighbours simplified at the stricter scale) is taken before commit 5.\n"
    );
    let _ = writeln!(
        r,
        "| level km/pt | max ratio | worst displacement pt | where |\n|---|---|---|---|"
    );
    for (k, p) in built.p2.iter().enumerate() {
        let _ = writeln!(
            r,
            "| {} | {:.4} | {:.4} | {} |",
            LADDER[k], p.max_ratio, p.worst_pt, p.worst_at
        );
    }
    let _ = writeln!(r);

    // R9
    let _ = writeln!(r, "## R9 — the seam\n");
    for (a3, s) in &inp.world.stitched {
        let _ = writeln!(
            r,
            "- **{a3}**: {} parts on the seam; parts {} → {}; {} vertices removed; {} notches \
             closed (longest {:.3} km)",
            s.seam_parts, s.parts_in, s.parts_out, s.removed_vertices, s.notches, s.notch_max_km
        );
    }
    let _ = writeln!(
        r,
        "\nSeam edges left in any unit: {} (must be 0).\n",
        inp.world.seam_edges_left
    );

    // D2
    let _ = writeln!(r, "## D2 — subdivisions as interior borders\n");
    let _ = writeln!(
        r,
        "The gate as planned: every once-found edge within 1 m of the country's admin-0 rings, no \
         edge found 3+ times. A country failing it is stored as the prototype's polygons (the \
         plan's fallback). `inside` counts the far once-edges whose midpoint lies inside the \
         country's admin-0 land — an interior border found on one side only, the failure the gate \
         is for — and their farthest distance.\n"
    );
    let _ = writeln!(
        r,
        "| code | admin 1 | edges | once | twice (borders) | 3+ | seam | lines | far once (> 1 m) | farthest m | inside | inside farthest m | gate | stored as |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for b in borders {
        let c = &b.census;
        let storage = built
            .subs
            .iter()
            .find(|s| inp.plans.get(s.country).map(|p| &p.code) == Some(&b.code))
            .map(|s| match s.storage {
                SubStorage::Borders => "borders",
                SubStorage::Polygons => "polygons",
            })
            .unwrap_or("-");
        let _ = writeln!(
            r,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {} | {storage} |",
            b.code,
            b.admin1,
            c.edges_total,
            c.once.len(),
            c.twice,
            c.thrice_or_more,
            c.seam,
            c.lines.len(),
            b.gate_far,
            if b.gate_worst_km.is_finite() {
                format!("{:.1}", b.gate_worst_km * 1000.0)
            } else {
                "> 10 000".into()
            },
            b.far_inside,
            b.far_inside_worst_km * 1000.0,
            if b.passes() { "PASS" } else { "FAIL" }
        );
    }
    let once: usize = borders.iter().map(|b| b.census.once.len()).sum();
    let twice: usize = borders.iter().map(|b| b.census.twice).sum();
    let _ = writeln!(
        r,
        "\nEdge-match shares over the 18: {twice} edges found twice (interior borders), {once} \
         once (the outline), {} along the seam; the gate passes for {} of {}.\n",
        borders.iter().map(|b| b.census.seam).sum::<usize>(),
        borders.iter().filter(|b| b.passes()).count(),
        borders.len()
    );

    // D1
    let _ = writeln!(r, "## D1 — raw or deflated\n");
    match bench {
        Some(b) => {
            let _ = writeln!(
                r,
                "`Store::load` from a file (read + parse + inflate + CRC), release build, 10 \
                 warm-ups + 100 runs; the clock reads in {:.0} ns.\n\n| encoding | bytes | load median ms | p90 ms |\n|---|---|---|---|\n| raw | {} | {:.2} | {:.2} |\n| deflated per blob | {} | {:.2} | {:.2} |\n\nThe rule: deflate unless its load exceeds {} ms → **{:?}**.\n",
                b.clock_ns,
                b.raw.0,
                b.raw.1,
                b.raw.2,
                b.deflate.0,
                b.deflate.1,
                b.deflate.2,
                crate::bench::RULE_MS,
                b.choice()
            );
        }
        None => {
            let _ = writeln!(r, "Not run (`--bench`).\n");
        }
    }

    // insets and S4
    let _ = writeln!(r, "## Insets (S6)\n");
    let _ = writeln!(
        r,
        "| code | label | box (x, y, w, h) | km/pt | level | clearance pt |\n|---|---|---|---|---|---|"
    );
    for p in &inp.plans {
        for i in &p.insets {
            let [x, y, w, h] = i.row.rect;
            let _ = writeln!(
                r,
                "| {} | {} | {x}, {y}, {w}, {h} | {:.3} | {} | {:.1} |",
                p.code,
                i.row.label,
                i.scale,
                LADDER[ondar_map::rules::level_for(i.scale)],
                i.clearance_pt
            );
        }
    }
    let _ = writeln!(r, "\n## S4 — map units\n");
    for (code, hits, n) in crate::world::s4_matches(&inp.world, &inp.aliases) {
        let _ = writeln!(
            r,
            "- {code}: {} of {n} parts matched in the parent",
            hits.len()
        );
    }

    // pins
    let _ = writeln!(
        r,
        "\n## Inputs (pinned)\n\n| file | bytes | SHA-256 |\n|---|---|---|"
    );
    if let Ok(pins) = crate::pins::parse(crate::pins::PINS_TSV) {
        for p in pins {
            let _ = writeln!(
                r,
                "| {} | {} | `{}` |",
                p.file,
                p.bytes,
                crate::pins::hex(&p.sha256)
            );
        }
    }
    r
}
