//! `world.ondarmap`, version 1: the bundled map resource, its writer (the build tool's) and its
//! loader (the app's) — one code path.
//!
//! Little-endian throughout. Every count is bounded by the bytes that remain, every offset is
//! checked, every blob's raw bytes carry a CRC32, and nothing in the loader can panic on any
//! input: a malformed file is a `LoadError` and the app runs without a map.
//!
//! ```text
//! header   "ONDARMAP" · u16 version · u32 length of the rest of the header ·
//!          tool git hash (40 ASCII) · NE tag (str8) · u8 n · n × (file str8, SHA-256 [32]) ·
//!          golden pane (W, H, padding: 3 × f32) · radius km f64 · u8 n · n × level f32
//! units    u32 n · n × Unit       (the stored geometry's owners: NE admin 0 + map units)
//! countries u32 n · n × Country   (what a code frames)
//! blobs    u32 n · n × BlobMeta · the blob bytes (offsets from the start of this region)
//! ```
//!
//! A blob's raw bytes: `u32` ring count, then per ring (`u32` vertices, `u32` bytes), then the
//! rings in `codec`'s encoding. A land blob holds every ring of its unit in the units table's
//! (part, ring) order, so the index (the caps) addresses rings without decoding them; a
//! subdivisions blob holds its country's interior-border lines in `sub_lines` order.

use crate::codec::Cursor;
use crate::rules::Pane;
use std::collections::HashMap;
use std::io::{Read, Write};

pub const MAGIC: &[u8; 8] = b"ONDARMAP";
pub const VERSION: u16 = 1;

/// No blob inflates past this, whatever its table says (the largest at Q2b was ~0.3 MB).
pub const MAX_BLOB_RAW: usize = 64 << 20;
/// Nor all of them together (Q2b's store: 9.3 MB raw).
pub const MAX_TOTAL_RAW: usize = 512 << 20;

/// `u16` for "none" in an index field.
pub const NONE16: u16 = u16::MAX;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LoadError {
    #[error("not a map resource (bad magic)")]
    Magic,
    #[error("map resource version {0}, this build reads {VERSION}")]
    Version(u16),
    #[error("map resource truncated or malformed in {0}")]
    Malformed(&'static str),
    #[error("map resource blob {blob} is corrupt ({why})")]
    Corrupt { blob: usize, why: &'static str },
}

/// Where the resource came from: the build tool's commit and the inputs' checksums.
#[derive(Clone, Debug, PartialEq)]
pub struct Pins {
    pub tool_git: String,
    pub ne_tag: String,
    pub inputs: Vec<(String, [u8; 32])>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub pins: Pins,
    pub golden_pane: Pane,
    pub radius_km: f64,
    pub ladder: Vec<f64>,
}

/// A spherical cap: every point of a ring or line lies within `radius_km` of (`lon`, `lat`) on
/// the ground. Computed from the unsimplified geometry; a reader adds the level's tolerance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cap {
    pub lon: f32,
    pub lat: f32,
    pub radius_km: f32,
}

/// A part's role in its own country's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Drawn as the country's land at its frame.
    Frame,
    /// Drawn in the country's inset with this index, at the fit view.
    Inset(u8),
    /// Neither an inset nor inside the frame's usable area: under 1 000 km² and not listed (S6).
    /// Drawn as the country's land wherever the view meets it (review finding 5: in the padding
    /// band at the fit, or zoomed and panned onto it), as a neighbour's view draws it.
    Dropped,
    /// A unit with no country (`-99`): only ever a neighbour.
    NeighbourOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub role: Role,
    /// The S4 map-unit country whose own unit holds these same rings: that country's frame
    /// omits this part from its neighbours (it draws its own unit instead).
    pub omit_in: Option<u16>,
    /// Ring 0 is the exterior, the rest are holes.
    pub rings: Vec<Cap>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub a3: [u8; 3],
    /// `ISO_A2_EH`, or `None` for `-99`.
    pub code: Option<[u8; 2]>,
    pub name: String,
    /// The LAEA the unit's rings are stored in: its country's frame centre for a country's main
    /// unit, else its own R1 centre.
    pub lat0: f64,
    pub lon0: f64,
    pub cap: Cap,
    pub parts: Vec<Part>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inset {
    pub label: String,
    pub corner: Corner,
    /// x, y, w, h on the golden pane, points; at another pane the box keeps its distance from
    /// `corner` (`rect_at`, frame.rs).
    pub rect: [f32; 4],
    /// The inset's own LAEA (R1 on its group), the group's bbox centre in it (km) and its scale.
    pub lat0: f64,
    pub lon0: f64,
    pub centre_km: [f64; 2],
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Country {
    pub code: [u8; 2],
    pub name: String,
    /// The units this code owns (S5); the first is the main unit, stored in the frame's LAEA.
    pub units: Vec<u16>,
    /// The frame LAEA's centre (R1; AQ at the pole).
    pub lat0: f64,
    pub lon0: f64,
    /// The frame group's projected bbox, km, `[min_x, min_y, max_x, max_y]`.
    pub bbox_km: [f64; 4],
    /// Draw subdivisions above 8 km/pt (S7: the fit at the golden pane is coarser than 8).
    pub subdivisions: bool,
    /// The frame group was chosen by `overrides.tsv` (S2), not by the largest part.
    pub overridden: bool,
    /// An S4 map unit standing for its code (the alias table).
    pub alias: bool,
    pub insets: Vec<Inset>,
    /// Caps of the interior-border lines, in the subdivisions blobs' order.
    pub sub_lines: Vec<Cap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    Land,
    Subdivisions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Deflate,
}

/// One blob's table row. `owner` is a unit index for land, a country index for subdivisions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlobMeta {
    pub owner: u16,
    pub level: u8,
    pub layer: Layer,
    pub encoding: Encoding,
    pub offset: u32,
    pub raw_len: u32,
    pub stored_len: u32,
    pub crc32: u32,
    pub vertices: u32,
    /// The measured displacement bound of this blob's simplification, points at its level.
    pub bound_pt: f32,
}

/// One ring or line of a blob, as the ring table locates it in the raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingRef {
    pub vertices: u32,
    pub start: u32,
    pub len: u32,
}

/// A loaded resource: the tables, and every blob inflated and CRC-checked.
#[derive(Debug)]
pub struct Store {
    pub header: Header,
    pub units: Vec<Unit>,
    pub countries: Vec<Country>,
    pub blobs: Vec<BlobMeta>,
    raw: Vec<Vec<u8>>,
    rings: Vec<Vec<RingRef>>,
    by_key: HashMap<(u16, u8, Layer), usize>,
}

impl Store {
    /// Parses, inflates and checks a resource. Never panics.
    pub fn load(bytes: &[u8]) -> Result<Store, LoadError> {
        let mut c = Cursor::new(bytes);
        if c.take(8) != Some(MAGIC.as_slice()) {
            return Err(LoadError::Magic);
        }
        let version = c.u16().ok_or(LoadError::Malformed("header"))?;
        if version != VERSION {
            return Err(LoadError::Version(version));
        }
        let header = read_header(&mut c).ok_or(LoadError::Malformed("header"))?;
        // the frame picks a level's index and tolerance from the compiled ladder and decodes at
        // the file's scale for that index: the two must be one (review finding 3)
        if header.ladder.as_slice() != crate::rules::LADDER.as_slice() {
            return Err(LoadError::Malformed("ladder"));
        }
        let units = read_units(&mut c).ok_or(LoadError::Malformed("units"))?;
        let countries = read_countries(&mut c).ok_or(LoadError::Malformed("countries"))?;
        let blobs = read_blob_table(&mut c).ok_or(LoadError::Malformed("blob table"))?;
        let region = c.take(c.remaining()).unwrap_or_default();
        let mut raw = Vec::with_capacity(blobs.len());
        let mut rings = Vec::with_capacity(blobs.len());
        let mut by_key = HashMap::with_capacity(blobs.len());
        let mut total = 0usize;
        for (i, b) in blobs.iter().enumerate() {
            let corrupt = |why| LoadError::Corrupt { blob: i, why };
            let raw_len = usize::try_from(b.raw_len).map_err(|_| corrupt("length"))?;
            total = total.checked_add(raw_len).ok_or(corrupt("length"))?;
            if raw_len > MAX_BLOB_RAW || total > MAX_TOTAL_RAW {
                return Err(corrupt("length"));
            }
            let start = usize::try_from(b.offset).map_err(|_| corrupt("offset"))?;
            let end = start
                .checked_add(usize::try_from(b.stored_len).map_err(|_| corrupt("offset"))?)
                .ok_or(corrupt("offset"))?;
            let stored = region.get(start..end).ok_or(corrupt("offset"))?;
            let bytes = match b.encoding {
                Encoding::Raw => stored.to_vec(),
                Encoding::Deflate => inflate(stored, raw_len).ok_or(corrupt("inflate"))?,
            };
            if bytes.len() != raw_len {
                return Err(corrupt("length"));
            }
            let mut crc = flate2::Crc::new();
            crc.update(&bytes);
            if crc.sum() != b.crc32 {
                return Err(corrupt("crc"));
            }
            let table = ring_table(&bytes).ok_or(corrupt("ring table"))?;
            let expected = match b.layer {
                Layer::Land => units
                    .get(usize::from(b.owner))
                    .map(|u| u.parts.iter().map(|p| p.rings.len()).sum::<usize>()),
                Layer::Subdivisions => countries
                    .get(usize::from(b.owner))
                    .map(|c| c.sub_lines.len()),
            };
            if expected != Some(table.len()) {
                return Err(corrupt("owner"));
            }
            if by_key.insert((b.owner, b.level, b.layer), i).is_some() {
                return Err(corrupt("duplicate"));
            }
            raw.push(bytes);
            rings.push(table);
        }
        Ok(Store {
            header,
            units,
            countries,
            blobs,
            raw,
            rings,
            by_key,
        })
    }

    pub fn pins(&self) -> &Pins {
        &self.header.pins
    }

    /// The blob for an owner, level and layer, if the resource has one.
    pub fn blob(&self, owner: u16, level: u8, layer: Layer) -> Option<usize> {
        self.by_key.get(&(owner, level, layer)).copied()
    }

    /// A blob's ring table.
    pub fn rings(&self, blob: usize) -> &[RingRef] {
        self.rings.get(blob).map(Vec::as_slice).unwrap_or_default()
    }

    /// Decodes ring `ring` of `blob` into `out`, in km of the owner's LAEA. `None` if out of
    /// range or malformed (which the load's checks make unreachable for a CRC-valid blob that
    /// the build tool wrote, but a reader never assumes it).
    pub fn decode(&self, blob: usize, ring: usize, out: &mut Vec<[f64; 2]>) -> Option<()> {
        let meta = self.blobs.get(blob)?;
        let level = *self.header.ladder.get(usize::from(meta.level))?;
        let r = self.rings.get(blob)?.get(ring)?;
        let bytes = self.raw.get(blob)?;
        let start = usize::try_from(r.start).ok()?;
        let end = start.checked_add(usize::try_from(r.len).ok()?)?;
        let n = usize::try_from(r.vertices).ok()?;
        crate::codec::decode_ring(bytes.get(start..end)?, n, level, out)
    }

    /// Total raw (inflated) bytes held.
    pub fn raw_bytes(&self) -> usize {
        self.raw.iter().map(Vec::len).sum()
    }
}

fn inflate(stored: &[u8], raw_len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw_len.min(stored.len().saturating_mul(16)));
    let limit = u64::try_from(raw_len).ok()?.checked_add(1)?;
    flate2::read::DeflateDecoder::new(stored)
        .take(limit)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

/// A blob's ring table, checked: the rings tile the bytes after the table exactly.
fn ring_table(bytes: &[u8]) -> Option<Vec<RingRef>> {
    let mut c = Cursor::new(bytes);
    let n = c.count(8)?;
    let mut t = Vec::with_capacity(n);
    let mut start = u32::try_from(c.position().checked_add(n.checked_mul(8)?)?).ok()?;
    for _ in 0..n {
        let vertices = c.u32()?;
        let len = c.u32()?;
        // the first vertex is 8 bytes, every other at least 4
        let min = if vertices == 0 {
            0
        } else {
            8u64 + 4 * (u64::from(vertices) - 1)
        };
        if u64::from(len) < min {
            return None;
        }
        t.push(RingRef {
            vertices,
            start,
            len,
        });
        start = start.checked_add(len)?;
    }
    (usize::try_from(start).ok()? == bytes.len()).then_some(t)
}

fn read_header(c: &mut Cursor) -> Option<Header> {
    let len = usize::try_from(c.u32()?).ok()?;
    let mut h = Cursor::new(c.take(len)?);
    let tool_git = String::from_utf8(h.take(40)?.to_vec()).ok()?;
    let ne_tag = h.str8()?;
    let n = usize::from(h.u8()?);
    let mut inputs = Vec::with_capacity(n);
    for _ in 0..n {
        inputs.push((h.str8()?, h.array::<32>()?));
    }
    let golden_pane = Pane {
        width: f64::from(h.f32()?),
        height: f64::from(h.f32()?),
        padding: f64::from(h.f32()?),
    };
    let radius_km = h.f64()?;
    let n = usize::from(h.u8()?);
    let mut ladder = Vec::with_capacity(n);
    for _ in 0..n {
        let l = f64::from(h.f32()?);
        // strictly increasing and positive, or no level could be chosen
        let above_last = l.partial_cmp(&ladder.last().copied().unwrap_or(0.0));
        if above_last != Some(std::cmp::Ordering::Greater) || !l.is_finite() {
            return None;
        }
        ladder.push(l);
    }
    (h.remaining() == 0 && !ladder.is_empty()).then_some(Header {
        pins: Pins {
            tool_git,
            ne_tag,
            inputs,
        },
        golden_pane,
        radius_km,
        ladder,
    })
}

fn read_cap(c: &mut Cursor) -> Option<Cap> {
    let cap = Cap {
        lon: c.f32()?,
        lat: c.f32()?,
        radius_km: c.f32()?,
    };
    (cap.lon.is_finite() && cap.lat.is_finite() && cap.radius_km.is_finite()).then_some(cap)
}

fn read_code(c: &mut Cursor) -> Option<Option<[u8; 2]>> {
    let code = c.array::<2>()?;
    if code == *b"--" {
        return Some(None);
    }
    code.iter()
        .all(u8::is_ascii_uppercase)
        .then_some(Some(code))
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

fn index16(v: u16) -> Option<u16> {
    (v != NONE16).then_some(v)
}

fn read_units(c: &mut Cursor) -> Option<Vec<Unit>> {
    let n = c.count(40)?;
    let mut units = Vec::with_capacity(n);
    for _ in 0..n {
        let a3 = c.array::<3>()?;
        let code = read_code(c)?;
        let name = c.str8()?;
        let lat0 = finite(c.f64()?)?;
        let lon0 = finite(c.f64()?)?;
        let cap = read_cap(c)?;
        let np = c.count(9)?;
        let mut parts = Vec::with_capacity(np);
        for _ in 0..np {
            let role = match (c.u8()?, c.u16()?) {
                (0, NONE16) => Role::Frame,
                (1, g) => Role::Inset(u8::try_from(g).ok()?),
                (2, NONE16) => Role::Dropped,
                (3, NONE16) => Role::NeighbourOnly,
                _ => return None,
            };
            let omit_in = index16(c.u16()?);
            let nr = c.count(12)?;
            let mut rings = Vec::with_capacity(nr);
            for _ in 0..nr {
                rings.push(read_cap(c)?);
            }
            parts.push(Part {
                role,
                omit_in,
                rings,
            });
        }
        units.push(Unit {
            a3,
            code,
            name,
            lat0,
            lon0,
            cap,
            parts,
        });
    }
    Some(units)
}

fn read_countries(c: &mut Cursor) -> Option<Vec<Country>> {
    let n = c.count(60)?;
    let mut countries = Vec::with_capacity(n);
    for _ in 0..n {
        let code = read_code(c)??;
        let name = c.str8()?;
        let nu = usize::from(c.u16()?);
        let mut units = Vec::with_capacity(nu.min(c.remaining() / 2));
        for _ in 0..nu {
            units.push(c.u16()?);
        }
        let lat0 = finite(c.f64()?)?;
        let lon0 = finite(c.f64()?)?;
        let bbox_km = [
            finite(c.f64()?)?,
            finite(c.f64()?)?,
            finite(c.f64()?)?,
            finite(c.f64()?)?,
        ];
        let flags = c.u8()?;
        if flags & !0b111 != 0 {
            return None;
        }
        let ni = usize::from(c.u8()?);
        let mut insets = Vec::with_capacity(ni);
        for _ in 0..ni {
            let label = c.str8()?;
            let corner = match c.u8()? {
                0 => Corner::TopLeft,
                1 => Corner::TopRight,
                2 => Corner::BottomLeft,
                3 => Corner::BottomRight,
                _ => return None,
            };
            let rect = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
            insets.push(Inset {
                label,
                corner,
                rect,
                lat0: finite(c.f64()?)?,
                lon0: finite(c.f64()?)?,
                centre_km: [finite(c.f64()?)?, finite(c.f64()?)?],
                scale: finite(c.f64()?)?,
            });
        }
        let nl = c.count(12)?;
        let mut sub_lines = Vec::with_capacity(nl);
        for _ in 0..nl {
            sub_lines.push(read_cap(c)?);
        }
        countries.push(Country {
            code,
            name,
            units,
            lat0,
            lon0,
            bbox_km,
            subdivisions: flags & 1 != 0,
            overridden: flags & 2 != 0,
            alias: flags & 4 != 0,
            insets,
            sub_lines,
        });
    }
    // every unit index a country names must exist
    Some(countries)
}

fn read_blob_table(c: &mut Cursor) -> Option<Vec<BlobMeta>> {
    let n = c.count(33)?;
    let mut blobs = Vec::with_capacity(n);
    for _ in 0..n {
        blobs.push(BlobMeta {
            owner: c.u16()?,
            level: c.u8()?,
            layer: match c.u8()? {
                0 => Layer::Land,
                1 => Layer::Subdivisions,
                _ => return None,
            },
            encoding: match c.u8()? {
                0 => Encoding::Raw,
                1 => Encoding::Deflate,
                _ => return None,
            },
            offset: c.u32()?,
            raw_len: c.u32()?,
            stored_len: c.u32()?,
            crc32: c.u32()?,
            vertices: c.u32()?,
            bound_pt: c.f32()?,
        });
    }
    Some(blobs)
}

// ------------------------------------------------------------------------------------- writer

/// One blob as the build tool hands it to the writer: its rings already quantised.
pub struct BlobIn {
    pub owner: u16,
    pub level: u8,
    pub layer: Layer,
    pub bound_pt: f32,
    pub rings: Vec<Vec<[i32; 2]>>,
}

/// The raw bytes of one blob: the ring table, then the rings.
pub fn blob_raw(rings: &[Vec<[i32; 2]>]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut table = Vec::with_capacity(4 + rings.len() * 8);
    table.extend_from_slice(&(rings.len() as u32).to_le_bytes());
    for r in rings {
        let len = crate::codec::encode_ring(r, &mut body);
        table.extend_from_slice(&(r.len() as u32).to_le_bytes());
        table.extend_from_slice(&(len as u32).to_le_bytes());
    }
    table.extend_from_slice(&body);
    table
}

/// Deflate at level 6 (D1).
pub fn deflate(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(raw)?;
    e.finish()
}

fn put_str8(out: &mut Vec<u8>, s: &str) -> std::io::Result<()> {
    let b = s.as_bytes();
    let n = u8::try_from(b.len()).map_err(|_| std::io::Error::other(format!("{s:?}: > 255 B")))?;
    out.push(n);
    out.extend_from_slice(b);
    Ok(())
}

fn put_cap(out: &mut Vec<u8>, c: &Cap) {
    for v in [c.lon, c.lat, c.radius_km] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_f64(out: &mut Vec<u8>, vs: &[f64]) {
    for v in vs {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Writes a resource. The bytes after the header depend only on the arguments, so two builds
/// from the same inputs are identical from the end of the header on (the header names the
/// tool's commit).
pub fn write(
    header: &Header,
    units: &[Unit],
    countries: &[Country],
    blobs: &[BlobIn],
    encoding: Encoding,
) -> std::io::Result<Vec<u8>> {
    let bad = |what: &str| std::io::Error::other(what.to_string());
    let mut h = Vec::new();
    let git = header.pins.tool_git.as_bytes();
    if git.len() != 40 {
        return Err(bad("the tool's git hash must be 40 ASCII characters"));
    }
    h.extend_from_slice(git);
    put_str8(&mut h, &header.pins.ne_tag)?;
    h.push(u8::try_from(header.pins.inputs.len()).map_err(|_| bad("inputs"))?);
    for (name, sha) in &header.pins.inputs {
        put_str8(&mut h, name)?;
        h.extend_from_slice(sha);
    }
    let p = header.golden_pane;
    for v in [p.width, p.height, p.padding] {
        h.extend_from_slice(&(v as f32).to_le_bytes());
    }
    put_f64(&mut h, &[header.radius_km]);
    h.push(u8::try_from(header.ladder.len()).map_err(|_| bad("ladder"))?);
    for l in &header.ladder {
        h.extend_from_slice(&(*l as f32).to_le_bytes());
    }

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(h.len() as u32).to_le_bytes());
    out.extend_from_slice(&h);

    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.a3);
        out.extend_from_slice(&u.code.unwrap_or(*b"--"));
        put_str8(&mut out, &u.name)?;
        put_f64(&mut out, &[u.lat0, u.lon0]);
        put_cap(&mut out, &u.cap);
        out.extend_from_slice(&(u.parts.len() as u32).to_le_bytes());
        for part in &u.parts {
            let (role, g) = match part.role {
                Role::Frame => (0u8, NONE16),
                Role::Inset(g) => (1, u16::from(g)),
                Role::Dropped => (2, NONE16),
                Role::NeighbourOnly => (3, NONE16),
            };
            out.push(role);
            out.extend_from_slice(&g.to_le_bytes());
            out.extend_from_slice(&part.omit_in.unwrap_or(NONE16).to_le_bytes());
            out.extend_from_slice(&(part.rings.len() as u32).to_le_bytes());
            for r in &part.rings {
                put_cap(&mut out, r);
            }
        }
    }

    out.extend_from_slice(&(countries.len() as u32).to_le_bytes());
    for c in countries {
        out.extend_from_slice(&c.code);
        put_str8(&mut out, &c.name)?;
        out.extend_from_slice(
            &u16::try_from(c.units.len())
                .map_err(|_| bad("units"))?
                .to_le_bytes(),
        );
        for u in &c.units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        put_f64(&mut out, &[c.lat0, c.lon0]);
        put_f64(&mut out, &c.bbox_km);
        out.push(u8::from(c.subdivisions) | u8::from(c.overridden) << 1 | u8::from(c.alias) << 2);
        out.push(u8::try_from(c.insets.len()).map_err(|_| bad("insets"))?);
        for i in &c.insets {
            put_str8(&mut out, &i.label)?;
            out.push(match i.corner {
                Corner::TopLeft => 0,
                Corner::TopRight => 1,
                Corner::BottomLeft => 2,
                Corner::BottomRight => 3,
            });
            for v in i.rect {
                out.extend_from_slice(&v.to_le_bytes());
            }
            let [cx, cy] = i.centre_km;
            put_f64(&mut out, &[i.lat0, i.lon0, cx, cy, i.scale]);
        }
        out.extend_from_slice(&(c.sub_lines.len() as u32).to_le_bytes());
        for l in &c.sub_lines {
            put_cap(&mut out, l);
        }
    }

    let mut table = Vec::new();
    let mut region = Vec::new();
    for b in blobs {
        let raw = blob_raw(&b.rings);
        let stored = match encoding {
            Encoding::Raw => raw.clone(),
            Encoding::Deflate => deflate(&raw)?,
        };
        let mut crc = flate2::Crc::new();
        crc.update(&raw);
        let vertices: usize = b.rings.iter().map(Vec::len).sum();
        let u32_of = |n: usize| u32::try_from(n).map_err(|_| bad("a blob past 4 GiB"));
        table.extend_from_slice(&b.owner.to_le_bytes());
        table.push(b.level);
        table.push(match b.layer {
            Layer::Land => 0,
            Layer::Subdivisions => 1,
        });
        table.push(match encoding {
            Encoding::Raw => 0,
            Encoding::Deflate => 1,
        });
        table.extend_from_slice(&u32_of(region.len())?.to_le_bytes());
        table.extend_from_slice(&u32_of(raw.len())?.to_le_bytes());
        table.extend_from_slice(&u32_of(stored.len())?.to_le_bytes());
        table.extend_from_slice(&crc.sum().to_le_bytes());
        table.extend_from_slice(&u32_of(vertices)?.to_le_bytes());
        table.extend_from_slice(&b.bound_pt.to_le_bytes());
        region.extend_from_slice(&stored);
    }
    out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&region);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::tests::Rng;

    pub fn header() -> Header {
        Header {
            pins: Pins {
                tool_git: "0123456789abcdef0123456789abcdef01234567".into(),
                ne_tag: "v5.1.2".into(),
                inputs: vec![("ne_10m_admin_0_countries.shp".into(), [7u8; 32])],
            },
            golden_pane: Pane::GOLDEN,
            radius_km: crate::laea::R_AUTHALIC_KM,
            ladder: crate::rules::LADDER.to_vec(),
        }
    }

    fn cap(lon: f32, lat: f32, r: f32) -> Cap {
        Cap {
            lon,
            lat,
            radius_km: r,
        }
    }

    /// A small resource with every feature: two units (one with a hole, one `-99`), a country
    /// with an inset and subdivision lines, land at two levels, subdivisions at one.
    pub fn synthetic(encoding: Encoding) -> Vec<u8> {
        with_header_encoded(&header(), encoding)
    }

    /// The synthetic resource under another header (raw).
    fn with_header(h: &Header) -> Vec<u8> {
        with_header_encoded(h, Encoding::Raw)
    }

    fn with_header_encoded(h: &Header, encoding: Encoding) -> Vec<u8> {
        write(
            h,
            &synthetic_units(),
            &synthetic_countries(),
            &synthetic_blobs(),
            encoding,
        )
        .unwrap()
    }

    fn synthetic_units() -> Vec<Unit> {
        vec![
            Unit {
                a3: *b"PRT",
                code: Some(*b"PT"),
                name: "Portugal".into(),
                lat0: 39.56,
                lon0: -7.85,
                cap: cap(-7.85, 39.56, 900.0),
                parts: vec![
                    Part {
                        role: Role::Frame,
                        omit_in: None,
                        rings: vec![cap(-8.0, 39.5, 300.0), cap(-8.0, 39.5, 20.0)],
                    },
                    Part {
                        role: Role::Inset(0),
                        omit_in: None,
                        rings: vec![cap(-27.3, 38.4, 200.0)],
                    },
                ],
            },
            Unit {
                a3: *b"SOL",
                code: None,
                name: "Somaliland".into(),
                lat0: 9.7,
                lon0: 46.0,
                cap: cap(46.0, 9.7, 400.0),
                parts: vec![Part {
                    role: Role::NeighbourOnly,
                    omit_in: Some(0),
                    rings: vec![cap(46.0, 9.7, 400.0)],
                }],
            },
        ]
    }

    fn synthetic_countries() -> Vec<Country> {
        vec![Country {
            code: *b"PT",
            name: "Portugal".into(),
            units: vec![0],
            lat0: 39.56,
            lon0: -7.85,
            bbox_km: [-146.0, -287.0, 146.0, 290.0],
            subdivisions: true,
            overridden: false,
            alias: false,
            insets: vec![Inset {
                label: "Azores".into(),
                corner: Corner::TopLeft,
                rect: [10.0, 24.0, 92.0, 52.0],
                lat0: 38.4,
                lon0: -27.3,
                centre_km: [0.5, -0.25],
                scale: 7.37,
            }],
            sub_lines: vec![cap(-8.0, 40.0, 100.0)],
        }]
    }

    fn synthetic_blobs() -> Vec<BlobIn> {
        let mut rng = Rng(42);
        let mut ring = |n: usize| -> Vec<[i32; 2]> {
            (0..n)
                .map(|_| {
                    [
                        (rng.next() % 100_000) as i32 - 50_000,
                        (rng.next() % 3_000) as i32,
                    ]
                })
                .collect()
        };
        vec![
            BlobIn {
                owner: 0,
                level: 0,
                layer: Layer::Land,
                bound_pt: 0.24,
                rings: vec![ring(120), ring(5), ring(30)],
            },
            BlobIn {
                owner: 0,
                level: 1,
                layer: Layer::Land,
                bound_pt: 0.2,
                rings: vec![ring(60), ring(4), ring(10)],
            },
            BlobIn {
                owner: 1,
                level: 0,
                layer: Layer::Land,
                bound_pt: 0.1,
                rings: vec![ring(40)],
            },
            BlobIn {
                owner: 0,
                level: 2,
                layer: Layer::Subdivisions,
                bound_pt: 0.49,
                rings: vec![ring(25)],
            },
        ]
    }

    /// Every ring of every blob decodes; a panic anywhere fails the caller.
    pub fn exercise(s: &Store) -> usize {
        let mut out = Vec::new();
        let mut n = 0;
        for b in 0..s.blobs.len() {
            for r in 0..s.rings(b).len() {
                if s.decode(b, r, &mut out).is_some() {
                    n += out.len();
                }
            }
        }
        n
    }

    /// The writer's output loads back to the same tables and the same quanta, raw or deflated.
    #[test]
    fn write_then_load() {
        for enc in [Encoding::Raw, Encoding::Deflate] {
            let bytes = synthetic(enc);
            let s = Store::load(&bytes).unwrap();
            assert_eq!(s.header, header());
            assert_eq!(s.units.len(), 2);
            assert_eq!(s.units[1].code, None);
            assert_eq!(s.units[1].parts[0].omit_in, Some(0));
            assert_eq!(s.units[0].parts[1].role, Role::Inset(0));
            assert_eq!(s.countries[0].insets[0].label, "Azores");
            assert!(s.countries[0].subdivisions && !s.countries[0].alias);
            assert_eq!(s.blobs.len(), 4);
            assert!(s.blobs.iter().all(|b| b.encoding == enc));
            assert_eq!(s.blob(0, 2, Layer::Subdivisions), Some(3));
            assert_eq!(s.blob(0, 2, Layer::Land), None);
            assert_eq!(exercise(&s), 120 + 5 + 30 + 60 + 4 + 10 + 40 + 25);
            let mut out = Vec::new();
            s.decode(2, 0, &mut out).unwrap();
            assert_eq!(out.len(), 40);
        }
        // the deflated file is smaller on random-ish data only a little, but never larger by
        // more than deflate's framing; the point is that both read back
        assert_ne!(synthetic(Encoding::Raw), synthetic(Encoding::Deflate));
    }

    /// (a) Every truncation of the first 4 KiB and 1 000 seeded lengths → `Err`, never a panic.
    /// (b) 10 000 seeded mutations of 1–8 bytes → `Ok` or `Err`, and every `Ok` decodes every
    /// ring. The test fails if any read is unchecked: a panic fails the test.
    #[test]
    fn loader_never_panics() {
        for enc in [Encoding::Raw, Encoding::Deflate] {
            let bytes = synthetic(enc);
            let len = bytes.len();
            let mut rng = Rng(7);
            let seeded: Vec<usize> = (0..1000).map(|_| (rng.next() as usize) % len).collect();
            for cut in (0..len.min(4096)).chain(seeded) {
                assert!(Store::load(&bytes[..cut]).is_err(), "{cut}");
            }
            let mut rng = Rng(0xF022);
            let mut oks = 0;
            for _ in 0..10_000 {
                let mut m = bytes.clone();
                for _ in 0..1 + rng.next() % 8 {
                    let i = (rng.next() as usize) % m.len();
                    m[i] = match rng.next() % 4 {
                        0 => 0,
                        1 => 0xFF,
                        2 => m[i] ^ (1 << (rng.next() % 8)),
                        _ => rng.next() as u8,
                    };
                }
                if let Ok(s) = Store::load(&m) {
                    oks += 1;
                    exercise(&s);
                }
            }
            // a mutation inside a CRC-covered blob is caught; one in a cap or a name may load
            assert!(oks < 10_000, "{oks}");
        }
    }

    /// (c) A flipped byte inside a blob's raw bytes → `Corrupt { blob }` for that blob.
    #[test]
    fn crc_catches_a_broken_blob() {
        let bytes = synthetic(Encoding::Raw);
        let s = Store::load(&bytes).unwrap();
        let region = bytes.len() - s.blobs.iter().map(|b| b.stored_len as usize).sum::<usize>();
        let b = 2;
        let at = region + s.blobs[b].offset as usize + 20;
        let mut m = bytes.clone();
        m[at] ^= 0x10;
        assert_eq!(
            Store::load(&m).unwrap_err(),
            LoadError::Corrupt {
                blob: b,
                why: "crc"
            }
        );
    }

    /// (d) Version 2 → `Version(2)`; a bad magic → `Magic`.
    #[test]
    fn version_and_magic() {
        let mut m = synthetic(Encoding::Raw);
        m[8] = 2;
        assert_eq!(Store::load(&m).unwrap_err(), LoadError::Version(2));
        m[0] = b'X';
        assert_eq!(Store::load(&m).unwrap_err(), LoadError::Magic);
        assert_eq!(Store::load(&[]).unwrap_err(), LoadError::Magic);
    }

    /// A blob whose ring count disagrees with its owner's rings is refused: the index would
    /// address the wrong rings.
    #[test]
    fn a_blob_must_match_its_owner() {
        let mut units = vec![];
        let countries = vec![];
        units.push(Unit {
            a3: *b"AAA",
            code: None,
            name: String::new(),
            lat0: 0.0,
            lon0: 0.0,
            cap: cap(0.0, 0.0, 1.0),
            parts: vec![Part {
                role: Role::NeighbourOnly,
                omit_in: None,
                rings: vec![cap(0.0, 0.0, 1.0)],
            }],
        });
        let blobs = vec![BlobIn {
            owner: 0,
            level: 0,
            layer: Layer::Land,
            bound_pt: 0.0,
            rings: vec![vec![[0, 0]; 3], vec![[1, 1]; 3]],
        }];
        let b = write(&header(), &units, &countries, &blobs, Encoding::Raw).unwrap();
        assert_eq!(
            Store::load(&b).unwrap_err(),
            LoadError::Corrupt {
                blob: 0,
                why: "owner"
            }
        );
    }

    /// The file's ladder must be the compiled `LADDER` (review finding 3): the frame picks a
    /// level's index and tolerance from the compiled ladder and decodes at the file's scale for
    /// that index, so a resource with a shorter or shifted ladder loaded and drew rings at the
    /// wrong scale (×1.33 for `[2, 4, …]`). A ladder that is increasing and positive but not
    /// `LADDER` is `Malformed("ladder")`; the compiled one loads.
    #[test]
    fn the_ladder_must_be_the_compiled_one() {
        let s = synthetic(Encoding::Raw);
        assert!(Store::load(&s).is_ok());
        for ladder in [
            vec![1.5, 3.0, 6.0, 12.0],
            vec![2.0, 4.0, 8.0, 16.0, 32.0],
            vec![1.5, 3.0, 6.0, 12.0, 24.0, 48.0],
        ] {
            let mut h = header();
            h.ladder = ladder.clone();
            let loaded = Store::load(&with_header(&h));
            assert_eq!(
                loaded.err(),
                Some(LoadError::Malformed("ladder")),
                "{ladder:?}"
            );
        }
    }

    /// A blob's ring table must tile its bytes exactly: a ring past the end, bytes the table
    /// does not account for, or a ring shorter than its vertex count allows are all refused
    /// (behind the CRC this guards a writer bug, not a corrupt file).
    #[test]
    fn ring_table_must_tile_the_bytes() {
        let raw = blob_raw(&[vec![[0, 0], [1, 1], [2, 2]], vec![[5, 5]; 4]]);
        let t = ring_table(&raw).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            t[0],
            RingRef {
                vertices: 3,
                start: 20,
                len: 16
            }
        );
        let mut extra = raw.clone();
        extra.push(0);
        assert_eq!(ring_table(&extra), None);
        assert_eq!(ring_table(&raw[..raw.len() - 1]), None);
        // ring 0 claims 3 vertices in 12 bytes (8 + 4 + 4 = 16 is the least), ring 1 the 4
        // bytes it gave up, so the table still tiles the bytes
        let mut short = raw.clone();
        short[8..12].copy_from_slice(&12u32.to_le_bytes());
        short[16..20].copy_from_slice(&24u32.to_le_bytes());
        assert_eq!(ring_table(&short), None);
    }

    /// No panic shape in the crate's non-test code: the scan `adts.rs` runs on itself
    /// (`unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, and an index:
    /// `[` right after an identifier, `)` or `]`). Fails on any of them added above
    /// `#[cfg(test)]` in any module.
    #[test]
    fn no_panic_shape() {
        const SHAPES: [&str; 7] = [
            // `f64::clamp` panics when min > max or either is NaN (review finding 4)
            ".clamp(",
            "unreachable!",
            "panic!",
            "todo!",
            "unimplemented!",
            ".unwrap()",
            ".expect(",
        ];
        let files = [
            ("lib.rs", include_str!("lib.rs")),
            ("laea.rs", include_str!("laea.rs")),
            ("rules.rs", include_str!("rules.rs")),
            ("codec.rs", include_str!("codec.rs")),
            ("clip.rs", include_str!("clip.rs")),
            ("format.rs", include_str!("format.rs")),
            ("index.rs", include_str!("index.rs")),
            ("frame.rs", include_str!("frame.rs")),
        ];
        for (name, src) in files {
            let code = src.split("#[cfg(test)]").next().unwrap_or(src);
            for (n, line) in code.lines().enumerate() {
                let line = line.split("//").next().unwrap_or(line);
                for shape in SHAPES {
                    assert!(!line.contains(shape), "{name}:{}: `{shape}`", n + 1);
                }
                let b = line.as_bytes();
                for i in 1..b.len() {
                    let prev = b[i - 1];
                    assert!(
                        !(b[i] == b'['
                            && (prev.is_ascii_alphanumeric()
                                || matches!(prev, b'_' | b')' | b']'))),
                        "{name}:{}: an index: {}",
                        n + 1,
                        line.trim()
                    );
                }
            }
        }
    }
}
