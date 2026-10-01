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

/// RDP at the same tolerance, for P4's comparison only (its displacement is ≤ t by
/// construction; it does not keep a ring simple): the open ring's vertex count, and whether the
/// result is a valid ring (simple, at least three vertices).
pub fn rdp_ring(ring: &[Coord<f64>], t: f64) -> (usize, bool) {
    use geo::{Simplify, Validation};
    let p = Polygon::new(LineString::from(ring.to_vec()), vec![]).simplify(t);
    let n = p.exterior().0.len().saturating_sub(1);
    (n, n >= 3 && p.is_valid())
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

    /// Tuning meets the tolerance exactly measured, and keeps fewer vertices at a larger
    /// tolerance. Fails if the bisection accepts a bound above t.
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
