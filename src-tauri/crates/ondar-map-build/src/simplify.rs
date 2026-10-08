//! R3, per ring: VW-preserve's area ε bisected until the ring's measured displacement is within
//! the tolerance (0.25 pt × level for land, 0.5 pt × level for subdivisions, in km).
//!
//! The measure is Step 0's (Q2, verified in Q2b): every original vertex's distance to the
//! simplified line. During the bisection it is bounded in O(n) by each removed vertex's
//! distance to the segment spanning it (`span_bound`, never below the exact measure); the final
//! figure is the exact one (`exact`, grid-accelerated). VW keeps each ring simple — the topology
//! RDP loses — and rings are independent, so one ε per ring rather than per country (Q2b: one
//! ε per country kept 1.7–5.2× RDP's vertices).

use geo::{Coord, LineString, Polygon, SimplifyVwPreserve};
use std::collections::HashMap;

/// The bisection's evaluations: the first at t²·10⁻⁶, then ×4 up from t²/2 while within the
/// tolerance (to t²·10⁴), then 12 halvings in log ε — 14 to 16 per ring, as the prototype.
const HALVINGS: usize = 12;

pub fn seg_dist(p: Coord<f64>, a: Coord<f64>, b: Coord<f64>) -> f64 {
    crate::geom::seg_dist(p, a, b)
}

/// Each removed vertex's distance to the segment of `simp` spanning it; `None` if `simp` is not
/// an in-order subset of `orig` sharing its first vertex (VW never removes the first or last).
pub fn span_bound(orig: &[Coord<f64>], simp: &[Coord<f64>]) -> Option<f64> {
    if simp.len() < 2 || orig.len() < 2 {
        return Some(0.0);
    }
    if orig.first() != simp.first() || orig.last() != simp.last() {
        return None;
    }
    let mut worst = 0f64;
    let mut k = 0usize;
    let mut pending: Vec<Coord<f64>> = Vec::new();
    for &c in orig.iter().skip(1) {
        if k + 1 < simp.len() && c == simp[k + 1] {
            for p in pending.drain(..) {
                worst = worst.max(seg_dist(p, simp[k], simp[k + 1]));
            }
            k += 1;
        } else {
            pending.push(c);
        }
    }
    (pending.is_empty() && k + 1 == simp.len()).then_some(worst)
}

/// The exact measure: every original vertex's distance to the nearest segment of `simp` (the
/// other direction is 0: simplified vertices are original vertices). A uniform grid of `cell`
/// km holds the segments; long segments are checked always.
pub fn exact(orig: &[Coord<f64>], simp: &[Coord<f64>], cell: f64) -> f64 {
    if simp.len() < 2 {
        return orig
            .iter()
            .map(|p| simp.first().map_or(0.0, |q| (p.x - q.x).hypot(p.y - q.y)))
            .fold(0.0, f64::max);
    }
    let key = |x: f64, y: f64| ((x / cell).floor() as i64, (y / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut long = Vec::new();
    for i in 0..simp.len() - 1 {
        let (a, b) = (simp[i], simp[i + 1]);
        let (x0, y0) = key(a.x.min(b.x), a.y.min(b.y));
        let (x1, y1) = key(a.x.max(b.x), a.y.max(b.y));
        if (x1 - x0 + 1) * (y1 - y0 + 1) > 4096 {
            long.push(i);
            continue;
        }
        for gx in x0..=x1 {
            for gy in y0..=y1 {
                grid.entry((gx, gy)).or_default().push(i);
            }
        }
    }
    let mut worst = 0f64;
    for &p in orig {
        let (cx, cy) = key(p.x, p.y);
        let mut best = long
            .iter()
            .map(|&i| seg_dist(p, simp[i], simp[i + 1]))
            .fold(f64::INFINITY, f64::min);
        let mut r = 0i64;
        loop {
            for gx in cx - r..=cx + r {
                for gy in cy - r..=cy + r {
                    if (gx - cx).abs() != r && (gy - cy).abs() != r {
                        continue;
                    }
                    if let Some(v) = grid.get(&(gx, gy)) {
                        for &i in v {
                            best = best.min(seg_dist(p, simp[i], simp[i + 1]));
                        }
                    }
                }
            }
            if best <= r as f64 * cell || r > 100_000 {
                break;
            }
            r += 1;
        }
        worst = worst.max(best);
    }
    worst
}

#[cfg(test)]
pub fn exact_brute(orig: &[Coord<f64>], simp: &[Coord<f64>]) -> f64 {
    orig.iter()
        .map(|&p| {
            simp.windows(2)
                .map(|w| seg_dist(p, w[0], w[1]))
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0.0, f64::max)
}

/// One simplification of a closed ring (first = last) or an open line.
fn vw(line: &[Coord<f64>], closed: bool, eps: f64) -> Vec<Coord<f64>> {
    let ls = LineString::from(line.to_vec());
    if closed {
        Polygon::new(ls, vec![])
            .simplify_vw_preserve(eps)
            .exterior()
            .0
            .clone()
    } else {
        ls.simplify_vw_preserve(eps).0
    }
}

#[derive(Clone, Debug)]
pub struct Tuned {
    pub line: Vec<Coord<f64>>,
    /// The exact measure of the result, km.
    pub bound: f64,
    pub evaluations: usize,
}

/// The largest ε (geometric bisection) whose `span_bound` is within `t` km. A ring that cannot
/// be simplified within `t` even at t²·10⁻⁶ is kept whole (bound 0).
pub fn tune(line: &[Coord<f64>], closed: bool, t: f64) -> Tuned {
    let eval = |e: f64| {
        let s = vw(line, closed, e);
        let b = span_bound(line, &s).unwrap_or(f64::INFINITY);
        (s, b)
    };
    let whole = || Tuned {
        line: line.to_vec(),
        bound: 0.0,
        evaluations: 1,
    };
    if line.len() <= if closed { 5 } else { 2 } {
        return whole();
    }
    let mut n = 1;
    let mut lo = t * t * 1e-6;
    let (mut best, b) = eval(lo);
    if b > t {
        return whole();
    }
    let mut hi = t * t * 0.5;
    loop {
        let (s, b) = eval(hi);
        n += 1;
        if b <= t {
            lo = hi;
            best = s;
            hi *= 4.0;
            if hi > t * t * 1e4 {
                break;
            }
        } else {
            for _ in 0..HALVINGS {
                let mid = (lo * hi).sqrt();
                let (s, b) = eval(mid);
                n += 1;
                if b <= t {
                    lo = mid;
                    best = s;
                } else {
                    hi = mid;
                }
            }
            break;
        }
    }
    let bound = exact(line, &best, t.max(1e-9));
    Tuned {
        line: best,
        bound,
        evaluations: n,
    }
}

/// RDP at tolerance `t` (its displacement is ≤ t by construction; it does not keep a ring
/// simple). A closed ring stays closed; geo keeps at least four coordinates of a ring.
pub fn rdp(line: &[Coord<f64>], closed: bool, t: f64) -> Vec<Coord<f64>> {
    use geo::Simplify;
    let ls = LineString::from(line.to_vec());
    if closed {
        Polygon::new(ls, vec![]).simplify(t).exterior().0.clone()
    } else {
        ls.simplify(t).0
    }
}

fn turn(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>) -> bool {
    // a fold back at b: collinear and reversing
    let (u, v) = ((b.x - a.x, b.y - a.y), (c.x - b.x, c.y - b.y));
    u.0 * v.1 - u.1 * v.0 == 0.0 && u.0 * v.0 + u.1 * v.1 < 0.0
}

/// Whether a line is simple: no two non-adjacent segments meet, no segment folds back onto the
/// next, no zero-length segment; a closed ring (first = last) also needs three distinct vertices
/// and a non-zero area. Segments are bucketed in a uniform grid, so a ring of n vertices costs
/// about O(n) rather than geo's O(n²) `is_valid`.
pub fn is_simple(line: &[Coord<f64>], closed: bool) -> bool {
    let n = line.len();
    if n < 2 || line.windows(2).any(|w| w[0] == w[1]) {
        return false;
    }
    let segs = n - 1;
    if closed {
        if line.first() != line.last() || n < 4 {
            return false;
        }
        let mut distinct: Vec<(u64, u64)> = line[..segs]
            .iter()
            .map(|c| (c.x.to_bits(), c.y.to_bits()))
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        let area: f64 = line
            .windows(2)
            .map(|w| w[0].x * w[1].y - w[1].x * w[0].y)
            .sum();
        if distinct.len() < 3 || area == 0.0 || turn(line[segs - 1], line[0], line[1]) {
            return false;
        }
    }
    if line.windows(3).any(|w| turn(w[0], w[1], w[2])) {
        return false;
    }
    let adjacent = |i: usize, j: usize| j == i + 1 || (closed && i == 0 && j == segs - 1);
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for c in line {
        (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y));
    }
    let cell = ((x1 - x0).max(y1 - y0) / (segs as f64).sqrt()).max(1e-9);
    let key = |x: f64, y: f64| {
        (
            ((x - x0) / cell).floor() as i64,
            ((y - y0) / cell).floor() as i64,
        )
    };
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for i in 0..segs {
        let (a, b) = (line[i], line[i + 1]);
        let (kx0, ky0) = key(a.x.min(b.x), a.y.min(b.y));
        let (kx1, ky1) = key(a.x.max(b.x), a.y.max(b.y));
        for gx in kx0..=kx1 {
            for gy in ky0..=ky1 {
                grid.entry((gx, gy)).or_default().push(i);
            }
        }
    }
    for bucket in grid.values() {
        for (p, &i) in bucket.iter().enumerate() {
            for &j in &bucket[p + 1..] {
                let (i, j) = (i.min(j), i.max(j));
                if !adjacent(i, j)
                    && crate::geom::segments_cross(line[i], line[i + 1], line[j], line[j + 1])
                {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Rdp,
    /// RDP at `t / 2^k`, `k` in 1..=3, landed simple and within `t` with fewer vertices than VW.
    RdpRetry(u8),
    Vw,
}

pub struct Hybrid {
    pub chosen: Tuned,
    pub pick: Pick,
    /// Open vertex counts of the two candidates (a closed ring's repeated last one not counted).
    pub vw: usize,
    pub rdp: usize,
}

fn open_len(line: &[Coord<f64>], closed: bool) -> usize {
    line.len().saturating_sub(usize::from(closed))
}

/// The per-ring hybrid (P4, decided 2026-10-01; the retry M4c's): RDP at `t`, kept if the result
/// is simple and its exact measure is within `t`. Otherwise RDP again at `t/2`, `t/4`, `t/8`: the
/// first result that is simple and within `t` is kept iff it has fewer open vertices than this
/// ring's per-ring VW, else VW (M4a's stated rule, first taken in M4c). No repair step.
pub fn hybrid(line: &[Coord<f64>], closed: bool, t: f64) -> Hybrid {
    let vw = tune(line, closed, t);
    let r = rdp(line, closed, t);
    let (vw_n, rdp_n) = (open_len(&vw.line, closed), open_len(&r, closed));
    let within = |cand: &[Coord<f64>]| {
        if !is_simple(cand, closed) {
            return None;
        }
        let bound = exact(line, cand, t.max(1e-9));
        (bound <= t).then_some(bound)
    };
    let mut first = Some(r);
    for k in 0u8..=3 {
        let c = match first.take() {
            Some(c) => c,
            None => rdp(line, closed, t / f64::from(1u32 << k)),
        };
        let Some(bound) = within(&c) else {
            continue;
        };
        if k > 0 && open_len(&c, closed) >= vw_n {
            break;
        }
        return Hybrid {
            chosen: Tuned {
                line: c,
                bound,
                evaluations: vw.evaluations + 1 + usize::from(k),
            },
            pick: if k == 0 { Pick::Rdp } else { Pick::RdpRetry(k) },
            vw: vw_n,
            rdp: rdp_n,
        };
    }
    Hybrid {
        chosen: vw,
        pick: Pick::Vw,
        vw: vw_n,
        rdp: rdp_n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiggle(n: usize, amp: f64, seed: u64) -> Vec<Coord<f64>> {
        let mut s = seed;
        let mut v: Vec<Coord<f64>> = (0..n)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let a = i as f64 / n as f64 * std::f64::consts::TAU;
                let r = 100.0 + amp * ((s % 1000) as f64 / 1000.0 - 0.5);
                Coord {
                    x: r * a.cos(),
                    y: r * a.sin(),
                }
            })
            .collect();
        v.push(v[0]);
        v
    }

    /// The span bound is never below the exact measure, and the grid equals brute force —
    /// Step 0's instrument check (Q2b verify), on seeded rings at three tolerances. Fails if the
    /// span bound skips a removed vertex or the grid misses a cell.
    #[test]
    fn the_instruments_agree() {
        for seed in 1..8u64 {
            let ring = wiggle(800, 6.0, seed);
            for t in [0.2, 1.0, 4.0] {
                for f in [0.1, 0.5, 2.0] {
                    let s = vw(&ring, true, t * t * f);
                    let brute = exact_brute(&ring, &s);
                    let span = span_bound(&ring, &s).unwrap();
                    let grid = exact(&ring, &s, t);
                    assert!(span + 1e-12 >= brute, "span {span} < brute {brute}");
                    assert!((grid - brute).abs() < 1e-12, "grid {grid} ≠ brute {brute}");
                }
            }
        }
    }

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    /// A thin band 100 long, 20 tall: the bottom edge dips `dip` below its chord at x = 50, and
    /// the top edge comes down to an apex at (50, `apex`), just above the dip and below the
    /// bottom's chord, 20 below its own. Along the top, a spike 3 tall and 0.2 wide (area 0.3:
    /// it caps VW's area threshold) and shallow bumps `bump` tall and 10 wide (area ≥ 0.5, so VW
    /// keeps them while RDP below `bump`'s tolerance drops them). RDP at any ε above `dip`
    /// straightens the bottom, keeps the apex, and the ring crosses itself.
    fn band(dip: f64, apex: f64, bump: f64) -> Vec<Coord<f64>> {
        let mut v = vec![c(0.0, 0.0), c(50.0, -dip), c(100.0, 0.0), c(100.0, 20.0)];
        v.extend([c(80.1, 20.0), c(80.0, 23.0), c(79.9, 20.0)]);
        let top = |x: f64, i: i32| c(x, 20.0 + if i % 2 == 1 { bump } else { 0.0 });
        v.extend((1..=6).map(|i| top(80.0 - 5.0 * f64::from(i), i)));
        v.extend([c(50.0, apex), c(45.0, 20.0)]);
        v.extend((1..=8).map(|i| top(45.0 - 5.0 * f64::from(i), i)));
        v.extend([c(0.0, 20.0), c(0.0, 0.0)]);
        v
    }

    /// The fallback the retry cannot fix (M4c (b)): the band with its dip 0.1 below the chord,
    /// under every retry's ε (t/8 = 0.15 at t = 1.2), so RDP crosses itself at t, t/2, t/4 and t/8;
    /// the hybrid returns VW's ring (23 vertices, simple). RDP at t/8 has 10, fewer than VW's, so
    /// only the simplicity check refuses it. Fails if the hybrid keeps RDP or a retry without the
    /// simplicity check (`Rdp` / `RdpRetry(1)`).
    #[test]
    fn a_ring_no_retry_makes_simple_falls_back_to_vw() {
        let ring = band(0.1, -0.05, 0.1);
        assert!(is_simple(&ring, true));
        for k in 0..=3 {
            let r = rdp(&ring, true, 1.2 / f64::from(1u32 << k));
            assert!(
                !is_simple(&r, true),
                "RDP at t/{} gave a simple ring",
                1 << k
            );
            assert!(open_len(&r, true) < open_len(&tune(&ring, true, 1.2).line, true));
        }
        let h = hybrid(&ring, true, 1.2);
        assert_eq!(h.pick, Pick::Vw);
        assert_eq!(h.chosen.line, tune(&ring, true, 1.2).line);
        assert!(is_simple(&h.chosen.line, true));
        // a well-behaved ring keeps RDP's result at t
        let w = wiggle(400, 6.0, 9);
        let h = hybrid(&w, true, 1.0);
        assert_eq!(h.pick, Pick::Rdp);
        assert!(h.chosen.bound <= 1.0 && h.rdp <= h.vw);
    }

    /// The retry (M4c (b)): the band with its dip 0.4 — dropped by RDP at t = 1.2 and t/2 (the ring
    /// crosses), kept at t/4 = 0.3, where the ring is simple, within t, and has 11 vertices
    /// against VW's 24 (VW keeps the shallow bumps) → `RdpRetry(2)`, RDP's t/4 ring. And a retry
    /// that lands simple but not shorter is not kept: M4a's thin band (dip 1.0, apex 0.5 above
    /// it) is simple at t/2 with 6 vertices, VW's count → `Vw`. Fails with no retry (`Vw` for the
    /// first), or with the retry kept whatever its length (`RdpRetry(1)` for the second).
    #[test]
    fn a_ring_simple_at_a_quarter_takes_the_retry() {
        let ring = band(0.4, -0.2, 0.2);
        assert!(is_simple(&ring, true));
        let h = hybrid(&ring, true, 1.2);
        assert_eq!(h.pick, Pick::RdpRetry(2));
        assert_eq!(h.chosen.line, rdp(&ring, true, 0.3));
        assert!(is_simple(&h.chosen.line, true) && h.chosen.bound <= 1.2);
        assert_eq!((open_len(&h.chosen.line, true), h.vw), (11, 24));
        let thin = vec![
            c(0.0, 0.0),
            c(2.5, -0.5),
            c(5.0, -1.0),
            c(7.5, -0.5),
            c(10.0, 0.0),
            c(10.0, 2.0),
            c(5.0, -0.5),
            c(0.0, 2.0),
            c(0.0, 0.0),
        ];
        let half = rdp(&thin, true, 0.6);
        assert!(is_simple(&half, true) && !is_simple(&rdp(&thin, true, 1.2), true));
        let h = hybrid(&thin, true, 1.2);
        assert_eq!((open_len(&half, true), h.vw, h.pick), (6, 6, Pick::Vw));
    }

    /// The simplicity check: a bow tie, a spike folding back, a zero-area ring and a repeated
    /// vertex are not simple; a square and an open zigzag are; a long ring with two far-apart
    /// vertices swapped crosses itself across many grid cells. Fails with crossings, fold-backs
    /// or repeats unchecked; the area check and the hybrid's bound check are equivalent mutants.
    #[test]
    fn simplicity() {
        let sq = [
            c(0.0, 0.0),
            c(1.0, 0.0),
            c(1.0, 1.0),
            c(0.0, 1.0),
            c(0.0, 0.0),
        ];
        assert!(is_simple(&sq, true));
        let bow = [
            c(0.0, 0.0),
            c(1.0, 1.0),
            c(1.0, 0.0),
            c(0.0, 1.0),
            c(0.0, 0.0),
        ];
        assert!(!is_simple(&bow, true));
        let spike = [
            c(0.0, 0.0),
            c(2.0, 0.0),
            c(1.0, 0.0),
            c(1.0, 1.0),
            c(0.0, 0.0),
        ];
        assert!(!is_simple(&spike, true));
        let flat = [c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0), c(0.0, 0.0)];
        assert!(!is_simple(&flat, true));
        let rep = [
            c(0.0, 0.0),
            c(1.0, 0.0),
            c(1.0, 0.0),
            c(1.0, 1.0),
            c(0.0, 0.0),
        ];
        assert!(!is_simple(&rep, true));
        let zig: Vec<Coord<f64>> = (0..50).map(|i| c(f64::from(i), f64::from(i % 2))).collect();
        assert!(is_simple(&zig, false));
        let mut back = zig.clone();
        back.push(c(10.5, -1.0));
        assert!(!is_simple(&back, false));
        // open lines where only one check can see the fault: a fold-back with no third
        // segment to touch, and a zero-length segment between two others
        let fold = [c(0.0, 0.0), c(2.0, 0.0), c(1.0, 0.0)];
        assert!(!is_simple(&fold, false));
        let repeat = [c(0.0, 0.0), c(1.0, 0.0), c(1.0, 0.0)];
        assert!(!is_simple(&repeat, false));
        let ok = wiggle(3000, 2.0, 3);
        assert!(is_simple(&ok, true));
        let mut big = ok.clone();
        let n = big.len();
        big.swap(10, n / 2);
        assert!(!is_simple(&big, true));
    }

    /// Tuning meets the tolerance exactly measured, and keeps fewer vertices at a larger
    /// tolerance; an open line keeps both ends. Fails if the bisection or its expansion accepts
    /// a bound above t, or with no halvings.
    #[test]
    fn tune_meets_the_tolerance() {
        let ring = wiggle(2000, 8.0, 42);
        let mut last = usize::MAX;
        for t in [0.25, 1.0, 4.0] {
            let r = tune(&ring, true, t);
            // the bisection steers by the span bound; the exact measure may be well below it
            let span = span_bound(&ring, &r.line).unwrap();
            assert!(r.bound <= span && span <= t, "{} / {span} > {t}", r.bound);
            assert!(
                span > t * 0.8,
                "span {span} far below {t}: the bisection stopped early"
            );
            assert!(r.line.len() < last);
            assert_eq!(r.line.first(), r.line.last());
            assert!((14..=17).contains(&r.evaluations), "{}", r.evaluations);
            last = r.line.len();
        }
        // an open line keeps both ends
        let line: Vec<Coord<f64>> = ring[..500].to_vec();
        let r = tune(&line, false, 1.0);
        assert_eq!((r.line.first(), r.line.last()), (line.first(), line.last()));
        assert!(r.bound <= 1.0);
    }
}
