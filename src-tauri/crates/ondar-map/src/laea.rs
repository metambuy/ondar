//! Spherical Lambert azimuthal equal-area, hand-written (M4a Step 0, Q5: `proj4rs` 0.2's
//! spherical forward is wrong in y, `laea.rs:318` applies k' to the first term only).
//!
//! Snyder, USGS Professional Paper 1395 (1987): forward eqs. (24-2), (22-4), (22-5); inverse
//! (20-18), (24-16), (20-14), (20-15), as used in its Appendix A worked example, pp. 332–333.
//! Coordinates in km, y up (north). Checked against pyproj 3.6.1 and d3-geo 3.1.1 at 37 points
//! (`fixtures/laea-reference.tsv`).

/// The WGS84 **authalic** radius (the sphere of equal surface area), km — the right sphere for
/// an equal-area projection (Step 0 review, A7). The mean radius (2a + b) / 3 is 6 371.0088 km:
/// 0.25 ppm of scale away, 10 m at 60° from the centre.
pub const R_AUTHALIC_KM: f64 = 6371.0072;

/// One LAEA, centred on (`lat0`, `lon0`), degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Laea {
    pub lat0: f64,
    pub lon0: f64,
    pub r: f64,
    sin_p1: f64,
    cos_p1: f64,
}

impl Laea {
    /// On the authalic sphere.
    pub fn new(lat0: f64, lon0: f64) -> Self {
        Self::with_radius(lat0, lon0, R_AUTHALIC_KM)
    }

    pub fn with_radius(lat0: f64, lon0: f64, r: f64) -> Self {
        let (sin_p1, cos_p1) = lat0.to_radians().sin_cos();
        Laea {
            lat0,
            lon0,
            r,
            sin_p1,
            cos_p1,
        }
    }

    /// Forward, degrees → km. `None` at the antipode, where k' is undefined.
    pub fn fwd(&self, lon: f64, lat: f64) -> Option<[f64; 2]> {
        let (sp, cp) = lat.to_radians().sin_cos();
        let (sdl, cdl) = (lon - self.lon0).to_radians().sin_cos();
        let d = 1.0 + self.sin_p1 * sp + self.cos_p1 * cp * cdl;
        if d.is_nan() || d <= 1e-12 {
            return None;
        }
        let k = (2.0 / d).sqrt();
        Some([
            self.r * k * cp * sdl,
            self.r * k * (self.cos_p1 * sp - self.sin_p1 * cp * cdl),
        ])
    }

    /// Inverse, km → (lon, lat) degrees, lon in [−180, 180]. `None` outside the disc of radius 2R.
    pub fn inv(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let rho = x.hypot(y);
        if rho.is_nan() {
            return None;
        }
        if rho < 1e-15 {
            return Some((self.lon0, self.lat0));
        }
        let s = rho / (2.0 * self.r);
        if s > 1.0 {
            return None;
        }
        let c = 2.0 * s.asin();
        let (sc, cc) = c.sin_cos();
        // Snyder (20-14) and (20-15) as one unit vector: east, north and toward-the-centre
        // components in the centre's frame, rotated by φ1. The latitude by atan2 rather than
        // (20-14)'s asin, which loses half its digits near ±90° (asin's slope is unbounded at 1).
        let (east, north) = (sc * x / rho, sc * y / rho);
        let sin_lat = cc * self.sin_p1 + north * self.cos_p1;
        let cos_lat_cos_dl = cc * self.cos_p1 - north * self.sin_p1;
        let lat = sin_lat.atan2(east.hypot(cos_lat_cos_dl));
        let dl = east.atan2(cos_lat_cos_dl);
        Some((wrap_lon(self.lon0 + dl.to_degrees()), lat.to_degrees()))
    }
}

/// A longitude into [−180, 180].
pub fn wrap_lon(lon: f64) -> f64 {
    if (-180.0..=180.0).contains(&lon) {
        return lon;
    }
    let w = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if w == -180.0 && lon > 0.0 { 180.0 } else { w }
}

/// Great-circle distance on the authalic sphere, km.
pub fn haversine_km(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * R_AUTHALIC_KM * a.sqrt().min(1.0).asin()
}

/// The shortest longitude interval holding every longitude given (antimeridian-aware, R1):
/// `(west, east)`, `east` possibly above 180. The complement of the largest gap between
/// consecutive longitudes, the wrap-around gap included. `None` for no longitudes.
pub fn lon_interval(lons: &mut [f64]) -> Option<(f64, f64)> {
    lons.sort_by(f64::total_cmp);
    let first = *lons.first()?;
    let last = *lons.last()?;
    // the gap after the last longitude, wrapping to the first: the interval is then [first, last]
    let mut best_gap = first + 360.0 - last;
    let mut best = (first, last);
    for w in lons.windows(2) {
        if let [a, b] = *w
            && b - a > best_gap
        {
            best_gap = b - a;
            best = (b, a + 360.0);
        }
    }
    Some(best)
}

/// The midpoint of `lon_interval`, in [−180, 180].
pub fn lon_midpoint(lons: &mut [f64]) -> Option<f64> {
    let (w, e) = lon_interval(lons)?;
    Some(wrap_lon((w + e) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row {
        case: String,
        lat0: f64,
        lon0: f64,
        r_km: f64,
        lat: f64,
        lon: f64,
        pyproj: [f64; 2],
        d3: [f64; 2],
    }

    fn reference() -> Vec<Row> {
        let src = include_str!("../fixtures/laea-reference.tsv");
        src.lines()
            .skip(1)
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let n = |i: usize| f[i].parse::<f64>().unwrap();
                Row {
                    case: f[0].to_string(),
                    lat0: n(1),
                    lon0: n(2),
                    r_km: n(3) / 1000.0,
                    lat: n(4),
                    lon: n(5),
                    pyproj: [n(6) / 1000.0, n(7) / 1000.0],
                    d3: [n(8) / 1000.0, n(9) / 1000.0],
                }
            })
            .collect()
    }

    /// The 37 Q5 points, forward and inverse, within 1e-6 km (1 mm) of pyproj and of d3-geo.
    /// The three implementations agree to 1.2e-8 m, so 1 mm is 100× their spread and 10⁶× below
    /// the codec's 0.0354 pt quantum. The fixture's radius is 6 371 007.2 m, so the test also
    /// pins `R_AUTHALIC_KM`: the mean radius 6 371.0088 moves a point 60° out by ~10 m.
    /// Snyder's example (R = 3) to 1e-7 against the source's seven decimals.
    #[test]
    fn reference_table() {
        let rows = reference();
        assert_eq!(rows.len(), 37);
        for r in &rows {
            let snyder = r.case.starts_with("snyder");
            let l = if snyder {
                Laea::with_radius(r.lat0, r.lon0, r.r_km)
            } else {
                assert_eq!(r.r_km, R_AUTHALIC_KM, "{}", r.case);
                Laea::new(r.lat0, r.lon0)
            };
            let [x, y] = l.fwd(r.lon, r.lat).unwrap();
            if snyder {
                // the source prints x = −4.2339303, y = 4.0257775; pyproj's metres are R = 3 m
                let (sx, sy) = (-4.2339303, 4.0257775);
                assert!(
                    (x * 1000.0 - sx).abs() < 1e-7,
                    "{} x {}",
                    r.case,
                    x * 1000.0
                );
                assert!(
                    (y * 1000.0 - sy).abs() < 1e-7,
                    "{} y {}",
                    r.case,
                    y * 1000.0
                );
                continue;
            }
            for (who, [ex, ey]) in [("pyproj", r.pyproj), ("d3", r.d3)] {
                let d = (x - ex).hypot(y - ey);
                assert!(d <= 1e-6, "{} vs {who}: {d:e} km", r.case);
            }
            let (lon, lat) = l.inv(x, y).unwrap();
            let back = haversine_km(lon, lat, r.lon, r.lat);
            assert!(back <= 1e-6, "{} inverse: {back:e} km", r.case);
        }
    }

    #[test]
    fn lon_interval_wraps_the_antimeridian() {
        // {170, −170}: the short way is across 180°, not the 340° through 0°
        let (w, e) = lon_interval(&mut [170.0, -170.0]).unwrap();
        assert_eq!((w, e), (170.0, 190.0));
        assert_eq!(lon_midpoint(&mut [170.0, -170.0]), Some(180.0));
        assert_eq!(lon_interval(&mut [-10.0, 10.0]), Some((-10.0, 10.0)));
        assert_eq!(lon_midpoint(&mut [-10.0, 10.0]), Some(0.0));
        // a set spanning 300°: the one 60° gap (−160 to −100) is left out, the wrap's 50° is not
        let mut set = [-160.0, -100.0, -50.0, 0.0, 50.0, 100.0, 150.0];
        assert_eq!(lon_interval(&mut set), Some((-100.0, 200.0)));
        // Russia's shape: the bulk east of 20° and Chukotka west of −170°
        let mid = lon_midpoint(&mut [19.6, 60.0, 120.0, 179.9, -179.9, -169.0]).unwrap();
        assert!((mid - (19.6 + 191.0) / 2.0).abs() < 1e-9, "{mid}");
        assert_eq!(lon_interval(&mut []), None);
    }

    #[test]
    fn wrap_lon_into_range() {
        assert_eq!(wrap_lon(190.0), -170.0);
        assert_eq!(wrap_lon(-190.0), 170.0);
        assert_eq!(wrap_lon(180.0), 180.0);
        assert_eq!(wrap_lon(540.0), 180.0);
        assert_eq!(wrap_lon(-180.0), -180.0);
    }

    #[test]
    fn antipode_and_outside_are_none() {
        let l = Laea::new(0.0, 0.0);
        assert_eq!(l.fwd(180.0, 0.0), None);
        assert_eq!(l.inv(2.0 * R_AUTHALIC_KM + 1.0, 0.0), None);
        assert_eq!(l.fwd(f64::NAN, 0.0), None);
        assert_eq!(l.inv(f64::NAN, 0.0), None);
    }
}
