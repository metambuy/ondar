//! Clipping to an axis-aligned rectangle, hand-written so no polygon-boolean crate enters the
//! runtime: Sutherland–Hodgman for rings (O(n) per edge of the window; the window is convex),
//! Liang–Barsky per segment for polylines. A ring that crosses the window leaves edges along the
//! window's boundary; the window is the pane grown by a margin, so those edges are never seen.

/// A rectangle, `[min_x, min_y, max_x, max_y]`.
pub type Rect = [f64; 4];

#[derive(Clone, Copy)]
enum Edge {
    Left(f64),
    Right(f64),
    Bottom(f64),
    Top(f64),
}

impl Edge {
    fn inside(self, [x, y]: [f64; 2]) -> bool {
        match self {
            Edge::Left(v) => x >= v,
            Edge::Right(v) => x <= v,
            Edge::Bottom(v) => y >= v,
            Edge::Top(v) => y <= v,
        }
    }

    fn cross(self, [ax, ay]: [f64; 2], [bx, by]: [f64; 2]) -> [f64; 2] {
        match self {
            Edge::Left(v) | Edge::Right(v) => [v, ay + (v - ax) / (bx - ax) * (by - ay)],
            Edge::Bottom(v) | Edge::Top(v) => [ax + (v - ay) / (by - ay) * (bx - ax), v],
        }
    }
}

/// Whether two rectangles meet (touching counts).
pub fn meets(&[ax0, ay0, ax1, ay1]: &Rect, &[bx0, by0, bx1, by1]: &Rect) -> bool {
    ax0 <= bx1 && bx0 <= ax1 && ay0 <= by1 && by0 <= ay1
}

/// Whether `inner` lies inside `outer` (touching counts).
pub fn within(&[ix0, iy0, ix1, iy1]: &Rect, &[ox0, oy0, ox1, oy1]: &Rect) -> bool {
    ix0 >= ox0 && ix1 <= ox1 && iy0 >= oy0 && iy1 <= oy1
}

/// The bounding rectangle of some points; `None` for none.
pub fn bounds(points: &[[f64; 2]]) -> Option<Rect> {
    let mut it = points.iter();
    let &[x, y] = it.next()?;
    let [mut x0, mut y0, mut x1, mut y1] = [x, y, x, y];
    for &[x, y] in it {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    Some([x0, y0, x1, y1])
}

/// Clips one ring (open: the last vertex is not repeated) to `window`. The result is empty when
/// fewer than three vertices remain.
pub fn clip_ring(ring: &[[f64; 2]], window: &Rect) -> Vec<[f64; 2]> {
    let Some(b) = bounds(ring) else {
        return Vec::new();
    };
    if !meets(&b, window) {
        return Vec::new();
    }
    if within(&b, window) {
        return ring.to_vec();
    }
    let [x0, y0, x1, y1] = *window;
    let mut out = ring.to_vec();
    let mut input = Vec::with_capacity(ring.len());
    for edge in [
        Edge::Left(x0),
        Edge::Right(x1),
        Edge::Bottom(y0),
        Edge::Top(y1),
    ] {
        std::mem::swap(&mut out, &mut input);
        out.clear();
        let Some(&last) = input.last() else {
            break;
        };
        let mut prev = last;
        for &cur in &input {
            match (edge.inside(cur), edge.inside(prev)) {
                (true, true) => out.push(cur),
                (true, false) => {
                    out.push(edge.cross(prev, cur));
                    out.push(cur);
                }
                (false, true) => out.push(edge.cross(prev, cur)),
                (false, false) => {}
            }
            prev = cur;
        }
    }
    if out.len() < 3 { Vec::new() } else { out }
}

/// Clips a polyline to `window`, as the pieces that lie inside it (each at least two vertices).
pub fn clip_polyline(line: &[[f64; 2]], window: &Rect) -> Vec<Vec<[f64; 2]>> {
    let mut pieces = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    for w in line.windows(2) {
        let [a, b] = match *w {
            [a, b] => [a, b],
            _ => continue,
        };
        match liang_barsky(a, b, window) {
            Some((p, q)) => {
                if cur.last() != Some(&p) {
                    if cur.len() >= 2 {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    cur.push(p);
                }
                cur.push(q);
                if q != b {
                    pieces.push(std::mem::take(&mut cur));
                }
            }
            None => {
                if cur.len() >= 2 {
                    pieces.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        pieces.push(cur);
    }
    pieces
}

/// The part of segment `a`–`b` inside `window`, if any.
fn liang_barsky(a: [f64; 2], b: [f64; 2], window: &Rect) -> Option<([f64; 2], [f64; 2])> {
    let ([ax, ay], [bx, by]) = (a, b);
    let [x0, y0, x1, y1] = *window;
    let (dx, dy) = (bx - ax, by - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, ax - x0), (dx, x1 - ax), (-dy, ay - y0), (dy, y1 - ay)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            [ax + t * dx, ay + t * dy]
        }
    };
    Some((at(t0), at(t1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Rect = [0.0, 0.0, 10.0, 10.0];

    fn area(r: &[[f64; 2]]) -> f64 {
        let n = r.len();
        (0..n)
            .map(|i| {
                let (a, b) = (r[i], r[(i + 1) % n]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f64>()
            / 2.0
    }

    /// Rotate a ring so it starts at its smallest vertex, for comparison.
    fn canon(mut r: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
        let i = (0..r.len())
            .min_by(|&a, &b| r[a].partial_cmp(&r[b]).unwrap())
            .unwrap();
        r.rotate_left(i);
        r
    }

    /// The square [−5, 5]² against [0, 10]² is the quarter [0, 5]².
    #[test]
    fn a_square() {
        let sq = [[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]];
        let c = canon(clip_ring(&sq, &W));
        assert_eq!(c, vec![[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 5.0]]);
        assert_eq!(area(&c).abs(), 25.0);
    }

    /// A ring with a hole: each ring is clipped alone and the hole stays a hole (even-odd).
    #[test]
    fn a_ring_with_a_hole() {
        let outer = [[2.0, 2.0], [14.0, 2.0], [14.0, 8.0], [2.0, 8.0]];
        let hole = [[4.0, 4.0], [12.0, 4.0], [12.0, 6.0], [4.0, 6.0]];
        let o = clip_ring(&outer, &W);
        let h = clip_ring(&hole, &W);
        assert_eq!(area(&o).abs(), 8.0 * 6.0);
        assert_eq!(area(&h).abs(), 6.0 * 2.0);
        assert_eq!(
            canon(h),
            vec![[4.0, 4.0], [10.0, 4.0], [10.0, 6.0], [4.0, 6.0]]
        );
    }

    /// A triangle across the window's corner: the corner is a vertex of the result.
    #[test]
    fn a_ring_across_a_corner() {
        let tri = [[8.0, 4.0], [14.0, 10.0], [8.0, 16.0]];
        let c = clip_ring(&tri, &W);
        assert!(c.contains(&[10.0, 10.0]), "{c:?}");
        assert_eq!(
            canon(c),
            vec![[8.0, 4.0], [10.0, 6.0], [10.0, 10.0], [8.0, 10.0]]
        );
    }

    #[test]
    fn no_intersection_is_empty() {
        let far = [[20.0, 20.0], [30.0, 20.0], [30.0, 30.0]];
        assert!(clip_ring(&far, &W).is_empty());
        // its bbox meets the window, the triangle does not
        let skew = [[11.0, -5.0], [15.0, -5.0], [15.0, 20.0]];
        assert!(clip_ring(&skew, &W).is_empty());
        assert!(clip_ring(&[], &W).is_empty());
    }

    #[test]
    fn a_ring_containing_the_window_is_the_window() {
        let big = [[-50.0, -50.0], [50.0, -50.0], [50.0, 50.0], [-50.0, 50.0]];
        let c = canon(clip_ring(&big, &W));
        assert_eq!(c, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
    }

    #[test]
    fn a_ring_inside_is_unchanged() {
        let r = vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0]];
        assert_eq!(clip_ring(&r, &W), r);
    }

    /// A polyline leaving and re-entering the window is two pieces, cut at the boundary.
    #[test]
    fn a_polyline_in_pieces() {
        let line = [
            [1.0, 5.0],
            [5.0, 5.0],
            [15.0, 5.0],
            [15.0, 8.0],
            [5.0, 8.0],
            [5.0, 9.0],
        ];
        let p = clip_polyline(&line, &W);
        assert_eq!(
            p,
            vec![
                vec![[1.0, 5.0], [5.0, 5.0], [10.0, 5.0]],
                vec![[10.0, 8.0], [5.0, 8.0], [5.0, 9.0]],
            ]
        );
        assert!(clip_polyline(&[[20.0, 20.0], [30.0, 30.0]], &W).is_empty());
        // a segment crossing the whole window
        assert_eq!(
            clip_polyline(&[[-5.0, 5.0], [15.0, 5.0]], &W),
            vec![vec![[0.0, 5.0], [10.0, 5.0]]]
        );
    }
}
