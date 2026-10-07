//! The build report, written beside the resource: everything the chat checks at P3 — bytes per
//! layer and level, the measured bounds, R9's stitch, P2, P4, D2, coverage, D1, the build time,
//! the pins and the resource's SHA-256.

use crate::Inputs;
use crate::bench::Bench;
use crate::borders::CountryBorders;
use crate::store::{Built, QUANT_PT, SubStorage};
use ondar_map::format::{Encoding, Layer};
use ondar_map::index::CLIP_MARGIN_PT;
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
    let (h0, h1) = built.bands;
    let _ = writeln!(
        r,
        "D6 (decided 2026-10-01): the view stays inside the fit rectangle (the pane at the \
         widest scale, centred on the frame bbox). **Per band (M4b commit 3):** the pane is \
         328 × h for every integer h in {h0}..={h1}, each with its own fit and fit rectangle. A \
         unit is stored at level k when a ring's cap, grown by the level's tolerance (bound + \
         codec), meets some country's reach at k — the bounding rectangle, over every band whose \
         views can use k, of that band's fit rectangle grown by the {CLIP_MARGIN_PT} pt clip \
         margin at the coarsest scale that uses k — or it is in an inset at the inset's level.\n"
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
    let ba = &built.bound_added;
    let _ = writeln!(
        r,
        "**The bound against the exact union** (the union of the bands' reaches is not a \
         rectangle): the bounding rectangle asks for {} land blob(s) no single band's reach asks \
         for, {} B deflated of {} B ({:.2} %). The rule: over 5 % and the exact union is stored \
         instead — {}.\n",
        ba.blobs,
        ba.bytes,
        ba.total_bytes,
        100.0 * ba.bytes as f64 / ba.total_bytes.max(1) as f64,
        if ba.exact_stored {
            "**applied**, those blobs are not in this file"
        } else {
            "not applied, the bound's blobs are stored"
        }
    );
    let _ = writeln!(
        r,
        "**Collapsed rings** (fewer than three distinct quanta at the level; stored empty, no \
         frame can draw them): {}. Kept as empty rings rather than dropped (the commit 4 STOP's \
         decision 7): the loader requires a land blob's ring count to equal its unit's, part by \
         part (`Corrupt {{ owner }}`), because the ring index — the caps in the units table — \
         addresses a blob's rings by position without decoding them; dropping a ring from one \
         level's blob would need a per-blob ring map, and the slot costs 8 B in the ring table \
         ({} B raw here, before deflate).{}\n",
        built.collapsed.len(),
        built.collapsed.len() * 8,
        if built.collapsed.is_empty() {
            String::new()
        } else {
            // per unit, the count at each level: `MDV 1/0/4/45/112` reads as the Maldives' rings
            // collapsing at 1.5 / 3 / 6 / 12 / 24 km/pt
            let mut by_unit: std::collections::BTreeMap<String, [usize; LADDER.len()]> =
                Default::default();
            for &(u, k, _) in &built.collapsed {
                if let Some(slot) = by_unit
                    .entry(String::from_utf8_lossy(&built.units[u].a3).into_owned())
                    .or_default()
                    .get_mut(k)
                {
                    *slot += 1;
                }
            }
            let per_level: Vec<usize> = (0..LADDER.len())
                .map(|k| built.collapsed.iter().filter(|c| c.1 == k).count())
                .collect();
            format!(
                " By level {}: {}. By unit (counts at each level): {}",
                LADDER
                    .iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(" / "),
                per_level
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(" / "),
                by_unit
                    .iter()
                    .map(|(a3, ns)| format!(
                        "{a3} {}",
                        ns.iter()
                            .map(|n| n.to_string())
                            .collect::<Vec<_>>()
                            .join("/")
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    let _ = writeln!(
        r,
        "**Subdivision candidates at the shortest band** (flagged off at the golden fit, fit at \
         {h0} above {} km/pt; the flag is decided at the golden fit — an observation, not a rule \
         the build applies): {}.\n",
        ondar_map::rules::SUBDIVISIONS_ABOVE_KM_PER_PT,
        if built.subdivision_candidates.is_empty() {
            "none".to_string()
        } else {
            built.subdivision_candidates.join(", ")
        }
    );

    // I1 + C1 (M4b commit 4): the per-band inset scales, the corner table, the labels
    let _ = writeln!(r, "## Insets per band (I1, C1)\n");
    let _ = writeln!(
        r,
        "At every band height the controls' rect (`rules::controls_rect`, {} × {} pt, {} pt from the \
         bottom and right) is placed first; each inset row, in table order, takes the largest scale in \
         whole percent at which its box — the golden size scaled, the label strip and the pads not, \
         anchored at its corner with the row's gaps — is inside the pane, apart from every box placed \
         before it and ≥ {} pt from the land the frame draws there. The minimum is a land area of \
         {} × {} pt (box ≥ 36 × 28). Labels at the artifact's {} pt, 0.6 em a character and 0.3 em a space.\n",
        ondar_map::rules::CONTROLS_SIZE_PT[0],
        ondar_map::rules::CONTROLS_SIZE_PT[1],
        ondar_map::rules::CONTROLS_MARGIN_PT,
        ondar_map::rules::INSET_CLEARANCE_PT,
        ondar_map::rules::INSET_MIN_LAND_PT[0],
        ondar_map::rules::INSET_MIN_LAND_PT[1],
        ondar_map::rules::INSET_LABEL_FONT_PT
    );
    let _ = writeln!(
        r,
        "| inset | corner | min % (at) | 140 | 161 | 178 | 200 | 250 | 300 | label pt | inner 178 / 300 |\n|---|---|---|---|---|---|---|---|---|---|---|"
    );
    let at = |i: &crate::world::InsetPlan, h: u32| {
        i.scale_pct
            .get(usize::try_from(h - ondar_map::rules::BAND_FLOOR).unwrap_or(0))
            .copied()
            .unwrap_or(0)
    };
    let inner = |i: &crate::world::InsetPlan, h: u32| {
        let pct = at(i, h);
        if pct == 0 {
            "—".to_string()
        } else {
            format!(
                "{:.0}",
                ondar_map::rules::label_inner_width(ondar_map::rules::inset_box_at(
                    i.row.rect,
                    i.row.corner,
                    &ondar_map::rules::Pane::band(h),
                    f64::from(pct) / 100.0
                ))
            )
        }
    };
    for p in &inp.plans {
        for i in &p.insets {
            let (min_i, &min_pct) = i
                .scale_pct
                .iter()
                .enumerate()
                .min_by_key(|&(_, &p)| p)
                .unwrap_or((0, &0));
            let _ = writeln!(
                r,
                "| {} {} | {:?} | {min_pct} ({}) | {} | {} | {} | {} | {} | {} | {:.1} | {} / {} |",
                p.code,
                i.row.label,
                i.row.corner,
                ondar_map::rules::BAND_FLOOR + min_i as u32,
                at(i, 140),
                at(i, 161),
                at(i, 178),
                at(i, 200),
                at(i, 250),
                at(i, 300),
                ondar_map::rules::label_width_pt(&i.row.label),
                inner(i, 178),
                inner(i, 300)
            );
        }
    }
    let _ = writeln!(
        r,
        "\nThe corner table — each box alone after the controls, with its own gaps, at TL / TR / BL: \
         min % over the bands (at), % at 178, % at 300, the full box's clearance at 161 in pt.\n"
    );
    let _ = writeln!(
        r,
        "| inset | current | TL | TR | BL |\n|---|---|---|---|---|"
    );
    for p in &inp.plans {
        for i in &p.insets {
            let cell = |c: &crate::world::CornerChoice| {
                format!(
                    "{} ({}) · {} · {} · {:.1}",
                    c.min_pct, c.min_at, c.pct_178, c.pct_300, c.clearance_161
                )
            };
            let cells: Vec<String> = i.corners.iter().map(cell).collect();
            let _ = writeln!(
                r,
                "| {} {} | {:?} | {} |",
                p.code,
                i.row.label,
                i.row.corner,
                cells.join(" | ")
            );
        }
    }
    let _ = writeln!(
        r,
        "\n**The stacking rule** (commit 4b, decision 1): a box whose golden rect abuts another's row or \
         column at the same corner keeps the golden gap to that box's near edge as it shrinks (Hawaii \
         beside Alaska, Madeira under the Azores). First band each inset is drawn at: {}.\n",
        inp.plans
            .iter()
            .flat_map(|p| p.insets.iter().map(move |i| (p, i)))
            .map(|(p, i)| format!(
                "{} {} {}",
                p.code,
                i.row.label,
                crate::world::first_band(i).map_or("never".to_string(), |h| h.to_string())
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let gate = crate::world::ship_gate(&inp.plans);
    let _ = writeln!(
        r,
        "\nThe ship gate (an inset in `MAY_DROP_AT_178`, {:?}, may be dropped at 178 — decision 1, case (c)): {} inset(s) dropped at 178 or 300{}; {} label(s) wider than their box{}.\n",
        crate::world::MAY_DROP_AT_178,
        gate.dropped.len(),
        if gate.dropped.is_empty() {
            String::new()
        } else {
            format!(" — {}", gate.dropped.join("; "))
        },
        gate.wide_labels.len(),
        if gate.wide_labels.is_empty() {
            String::new()
        } else {
            format!(" — {}", gate.wide_labels.join("; "))
        }
    );

    // P4
    let _ = writeln!(r, "## P4 — the simplifier\n");
    let (vin, vw, rdp, chosen) = built.p4;
    let _ = writeln!(
        r,
        "Stored: **{:?}**. The hybrid (decided 2026-10-01): per ring and level, RDP at the level's \
         tolerance, kept if simple (no self-intersection, ≥ 3 distinct vertices, non-zero area) \
         and its exact measure is within the bound; else that ring's per-ring VW. No repair. The \
         rule: ship the hybrid if every bound holds and the build stays under ~10 min, else VW.\n",
        built.simplifier
    );
    let _ = writeln!(
        r,
        "RU land at 24 km/pt (open rings, RU's frame LAEA, after the seam stitch): **{vin} in; \
         per-ring VW {vw}, RDP {rdp}, stored {chosen}** (commit 4 measured VW 13 664, RDP 3 904; \
         Q2b one ε per country 16 667 against RDP 4 128). In that blob {} ring(s) fell back to \
         VW; the largest has {} vertices in, RDP {} (not simple, or over the bound), VW {}.\n",
        built.p4_fallback.0, built.p4_fallback.1.2, built.p4_fallback.1.0, built.p4_fallback.1.1
    );
    let _ = writeln!(
        r,
        "Open vertices before quantisation, summed over the stored blobs:\n\n| layer | level km/pt | rings | per-ring VW | RDP | stored | stored / RDP | rings that fell back to VW |\n|---|---|---|---|---|---|---|---|"
    );
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
            let sum = |f: fn(&&crate::store::BlobOut) -> usize| bs.iter().map(f).sum::<usize>();
            let (vw, rdp, ch) = (
                sum(|b| b.vertices_vw),
                sum(|b| b.vertices_rdp),
                sum(|b| b.vertices_chosen),
            );
            let _ = writeln!(
                r,
                "| {layer:?} | {l} | {} | {vw} | {rdp} | {ch} | {:.3} | {} |",
                sum(|b| b.rings.len()),
                ch as f64 / rdp.max(1) as f64,
                sum(|b| b.fallbacks)
            );
        }
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
        "The gate (decided 2026-10-01): every once-found edge whose midpoint lies inside the \
         country's admin-0 land — an interior border found on one side only — within {:.0} m of \
         the admin-0 rings, and no edge found 3+ times; a country failing it is stored as the \
         prototype's polygons. `inside` counts the once-edges more than 1 m from the rings whose \
         midpoint is inside the land (closer ones lie on the rings); `far once` counts every \
         once-edge more than 1 m away (coast differences between NE's two layers, islets admin 0 \
         lacks).\n",
        crate::borders::GATE_INSIDE_KM * 1000.0
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
    let (sub_raw, sub_def) = built
        .blobs
        .iter()
        .filter(|b| b.layer == Layer::Subdivisions)
        .map(|b| {
            let raw = ondar_map::format::blob_raw(&b.rings);
            (
                raw.len(),
                ondar_map::format::deflate(&raw).map_or(0, |d| d.len()),
            )
        })
        .fold((0, 0), |(a, b), (x, y)| (a + x, b + y));
    let _ = writeln!(
        r,
        "Subdivision bytes: {sub_def} B deflated ({sub_raw} B raw), against 527 856 B deflated \
         when 13 of the 18 were stored as polygons (commit 4, VW).\n"
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
