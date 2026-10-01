//! The ring codec: a ring's vertices as quanta of 0.05 pt at its level, the first vertex as two
//! `i32`, every other vertex as an `i16` delta pair; a delta outside `i16` (or exactly
//! `i16::MIN`) is the escape pair `(i16::MIN, i16::MIN)` followed by the vertex as two `i32`.
//! Lossless to half a quantum per axis: ≤ 0.0354 pt (√2 × 0.025). Absolute `i16` would span only
//! ±1 638 pt, which the deeper levels exceed (RU at the floor is ~8 000 pt wide), hence deltas.
//!
//! Decoding never panics: every read is bounded, and a short or overlong input is `None`.

/// Points per quantum, at the level's scale.
pub const QUANTUM_PT: f64 = 0.05;

const ESCAPE: i16 = i16::MIN;

/// A vertex (km) as quanta at a level of `level_km_per_pt`. `None` if it does not fit an `i32`.
pub fn quantise(p: [f64; 2], level_km_per_pt: f64) -> Option<[i32; 2]> {
    let q = level_km_per_pt * QUANTUM_PT;
    let [px, py] = p;
    let (x, y) = ((px / q).round(), (py / q).round());
    let range = f64::from(i32::MIN)..=f64::from(i32::MAX);
    if range.contains(&x) && range.contains(&y) {
        Some([x as i32, y as i32])
    } else {
        None
    }
}

/// Appends one ring's quanta to `out`; returns the bytes written.
pub fn encode_ring(ring: &[[i32; 2]], out: &mut Vec<u8>) -> usize {
    let start = out.len();
    let mut prev: Option<[i32; 2]> = None;
    for &[x, y] in ring {
        match prev {
            None => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Some([px, py]) => {
                let (dx, dy) = (i64::from(x) - i64::from(px), i64::from(y) - i64::from(py));
                match (i16::try_from(dx), i16::try_from(dy)) {
                    (Ok(dx), Ok(dy)) if dx != ESCAPE && dy != ESCAPE => {
                        out.extend_from_slice(&dx.to_le_bytes());
                        out.extend_from_slice(&dy.to_le_bytes());
                    }
                    _ => {
                        out.extend_from_slice(&ESCAPE.to_le_bytes());
                        out.extend_from_slice(&ESCAPE.to_le_bytes());
                        out.extend_from_slice(&x.to_le_bytes());
                        out.extend_from_slice(&y.to_le_bytes());
                    }
                }
            }
        }
        prev = Some([x, y]);
    }
    out.len() - start
}

/// A bounded little-endian reader over a byte slice: every read is an `Option`.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(u8::from_le_bytes(self.array()?))
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.array()?))
    }

    /// A `u32` count, bounded so that `count × min_item_bytes` fits in what remains.
    pub fn count(&mut self, min_item_bytes: usize) -> Option<usize> {
        let n = usize::try_from(self.u32()?).ok()?;
        (n.checked_mul(min_item_bytes.max(1))? <= self.remaining()).then_some(n)
    }

    /// A `u8`-length UTF-8 string.
    pub fn str8(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Decodes one ring of `n` vertices from exactly `bytes`, each vertex scaled to km at a level of
/// `level_km_per_pt`, into `out` (cleared first). `None` on a short or overlong input.
pub fn decode_ring(
    bytes: &[u8],
    n: usize,
    level_km_per_pt: f64,
    out: &mut Vec<[f64; 2]>,
) -> Option<()> {
    out.clear();
    out.reserve(n.min(bytes.len() / 4 + 1));
    let q = level_km_per_pt * QUANTUM_PT;
    let mut c = Cursor::new(bytes);
    let (mut x, mut y) = (0i64, 0i64);
    for k in 0..n {
        if k == 0 {
            x = i64::from(c.i32()?);
            y = i64::from(c.i32()?);
        } else {
            let (dx, dy) = (c.i16()?, c.i16()?);
            if dx == ESCAPE && dy == ESCAPE {
                x = i64::from(c.i32()?);
                y = i64::from(c.i32()?);
            } else {
                x += i64::from(dx);
                y += i64::from(dy);
            }
        }
        out.push([x as f64 * q, y as f64 * q]);
    }
    (c.remaining() == 0).then_some(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small seeded generator (xorshift64*): no dependency, the same sequence everywhere.
    pub struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        pub fn f(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn round_trip(ring_km: &[[f64; 2]], level: f64) -> Vec<[f64; 2]> {
        let q: Vec<[i32; 2]> = ring_km
            .iter()
            .map(|&p| quantise(p, level).unwrap())
            .collect();
        let mut b = Vec::new();
        let len = encode_ring(&q, &mut b);
        assert_eq!(len, b.len());
        let mut out = Vec::new();
        decode_ring(&b, q.len(), level, &mut out).unwrap();
        out
    }

    /// Seeded random rings — small steps, steps past `i16`, coordinates far from the origin —
    /// come back within half a quantum per axis: ≤ 0.0354 pt at the level. Fails on a delta's
    /// sign, the escape, or the quantum.
    #[test]
    fn round_trip_within_half_a_quantum() {
        let mut rng = Rng(0x0DDA_2026_1001);
        for level in [1.5, 3.0, 6.0, 12.0, 24.0] {
            for _ in 0..200 {
                let n = 1 + (rng.next() % 300) as usize;
                let big = rng.next().is_multiple_of(4);
                let (mut x, mut y) = ((rng.f() - 0.5) * 20_000.0, (rng.f() - 0.5) * 20_000.0);
                let mut ring = Vec::with_capacity(n);
                for _ in 0..n {
                    let step = if big { 5_000.0 } else { 5.0 };
                    x += (rng.f() - 0.5) * step;
                    y += (rng.f() - 0.5) * step;
                    ring.push([x, y]);
                }
                let back = round_trip(&ring, level);
                assert_eq!(back.len(), ring.len());
                for (a, b) in ring.iter().zip(&back) {
                    let d_pt = (a[0] - b[0]).hypot(a[1] - b[1]) / level;
                    assert!(d_pt <= 0.0354, "{d_pt} pt at level {level}");
                }
            }
        }
    }

    /// A delta of exactly `i16::MIN` on one axis is escaped, not read as half an escape pair;
    /// a delta one past `i16::MAX` is escaped; both decode exactly.
    #[test]
    fn the_escape_pair() {
        let ring = [
            [0, 0],
            [i32::from(i16::MIN), 5],
            [i32::from(i16::MIN), i32::from(i16::MIN) + 7],
            [40_000, 0],
            [40_001, -1],
        ];
        let mut b = Vec::new();
        encode_ring(&ring, &mut b);
        // first vertex 8 B, escapes 2 × 12 B, then one plain delta 4 B, then an escape 12 B
        // (40 000 − (−32 768) is past i16), then a plain delta 4 B
        assert_eq!(b.len(), 8 + 12 + 4 + 12 + 4);
        let mut out = Vec::new();
        decode_ring(&b, ring.len(), 20.0, &mut out).unwrap();
        let q = 20.0 * QUANTUM_PT;
        for (a, o) in ring.iter().zip(&out) {
            assert_eq!([f64::from(a[0]) * q, f64::from(a[1]) * q], *o);
        }
    }

    /// Every truncation of an encoded ring is `None`, never a panic; so are trailing bytes and
    /// a vertex count larger than the bytes hold.
    #[test]
    fn truncated_bytes_are_none() {
        let ring: Vec<[i32; 2]> = (0..50).map(|i| [i * 1000, -i * 70_000]).collect();
        let mut b = Vec::new();
        encode_ring(&ring, &mut b);
        let mut out = Vec::new();
        for cut in 0..b.len() {
            assert!(
                decode_ring(&b[..cut], ring.len(), 1.5, &mut out).is_none(),
                "{cut}"
            );
        }
        let mut long = b.clone();
        long.push(0);
        assert!(decode_ring(&long, ring.len(), 1.5, &mut out).is_none());
        assert!(decode_ring(&b, ring.len() + 1, 1.5, &mut out).is_none());
        assert!(decode_ring(&b, usize::MAX, 1.5, &mut out).is_none());
        assert!(decode_ring(&b, ring.len(), 1.5, &mut out).is_some());
    }

    #[test]
    fn quantise_out_of_range_is_none() {
        assert_eq!(quantise([1e12, 0.0], 1.5), None);
        assert_eq!(quantise([f64::NAN, 0.0], 1.5), None);
        assert_eq!(quantise([0.075, -0.075], 1.5), Some([1, -1]));
    }

    #[test]
    fn cursor_counts_are_bounded_by_what_remains() {
        let mut c = Cursor::new(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 2]);
        assert_eq!(c.count(1), None);
        let mut c = Cursor::new(&[2, 0, 0, 0, 1, 2]);
        assert_eq!(c.count(1), Some(2));
        let mut c = Cursor::new(&[2, 0, 0, 0, 1, 2]);
        assert_eq!(c.count(2), None);
        let mut c = Cursor::new(&[3, b'a', 0xFF, b'c']);
        assert_eq!(c.str8(), None);
    }
}
