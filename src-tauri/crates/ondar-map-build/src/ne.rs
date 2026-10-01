//! Natural Earth reader: shapefile geometry + dBASE attributes into lon/lat polygons. Each outer
//! ring with the holes that follow it is one part. Reads from bytes already checked by `pins`.

use geo::{MultiPolygon, Polygon};
use shapefile::dbase::{FieldValue, Record};
use std::io::Cursor;

#[derive(Clone, Debug)]
pub struct Unit {
    pub a3: String,
    /// `ISO_A2_EH` (Q6: `ISO_A2` is `-99` for FR, NO and seven more); `None` for `-99`.
    pub code: Option<String>,
    pub name: String,
    /// `ADM0_A3` of the admin-0 unit a map unit belongs to (its own `a3` for admin 0).
    pub parent_a3: String,
    pub parts: Vec<Polygon<f64>>,
}

#[derive(Clone, Debug)]
pub struct Admin1 {
    pub adm0_a3: String,
    pub name: String,
    pub parts: Vec<Polygon<f64>>,
}

fn text(rec: &Record, field: &str) -> String {
    match rec.get(field) {
        Some(FieldValue::Character(Some(s))) => s.trim_matches([' ', '\0']).to_string(),
        Some(FieldValue::Memo(s)) => s.trim_matches([' ', '\0']).to_string(),
        _ => String::new(),
    }
}

fn polys(shape: shapefile::Shape) -> Result<Vec<Polygon<f64>>, String> {
    match shape {
        shapefile::Shape::Polygon(p) => {
            let mp: MultiPolygon<f64> = MultiPolygon::try_from(p).map_err(|e| format!("{e:?}"))?;
            Ok(mp.0)
        }
        shapefile::Shape::NullShape => Ok(vec![]),
        other => Err(format!("unexpected shape type {}", other.shapetype())),
    }
}

/// The records of one layer from its `.shp`, `.shx` and `.dbf` bytes.
type Records = Vec<(Vec<Polygon<f64>>, Record)>;

fn records(shp: &[u8], shx: &[u8], dbf: &[u8]) -> Result<Records, String> {
    let shape_reader =
        shapefile::ShapeReader::with_shx(Cursor::new(shp.to_vec()), Cursor::new(shx.to_vec()))
            .map_err(|e| format!("{e:?}"))?;
    let dbf_reader =
        shapefile::dbase::Reader::new(Cursor::new(dbf.to_vec())).map_err(|e| format!("{e:?}"))?;
    let mut r = shapefile::Reader::new(shape_reader, dbf_reader);
    let mut out = Vec::new();
    for item in r.iter_shapes_and_records() {
        let (shape, rec) = item.map_err(|e| format!("{e:?}"))?;
        out.push((polys(shape)?, rec));
    }
    Ok(out)
}

pub struct Layer<'a> {
    pub shp: &'a [u8],
    pub shx: &'a [u8],
    pub dbf: &'a [u8],
}

fn code_of(s: String) -> Option<String> {
    (s != "-99" && !s.is_empty()).then_some(s)
}

pub fn admin0(l: Layer) -> Result<Vec<Unit>, String> {
    Ok(records(l.shp, l.shx, l.dbf)?
        .into_iter()
        .map(|(parts, rec)| Unit {
            a3: text(&rec, "ADM0_A3"),
            code: code_of(text(&rec, "ISO_A2_EH")),
            name: text(&rec, "NAME"),
            parent_a3: text(&rec, "ADM0_A3"),
            parts,
        })
        .collect())
}

/// The map units whose `GU_A3` is in `wanted`, in the file's order.
pub fn map_units(l: Layer, wanted: &[&str]) -> Result<Vec<Unit>, String> {
    Ok(records(l.shp, l.shx, l.dbf)?
        .into_iter()
        .filter_map(|(parts, rec)| {
            let gu = text(&rec, "GU_A3");
            wanted.contains(&gu.as_str()).then(|| Unit {
                a3: gu,
                code: code_of(text(&rec, "ISO_A2_EH")),
                name: text(&rec, "NAME"),
                parent_a3: text(&rec, "ADM0_A3"),
                parts,
            })
        })
        .collect())
}

pub fn admin1(l: Layer) -> Result<Vec<Admin1>, String> {
    Ok(records(l.shp, l.shx, l.dbf)?
        .into_iter()
        .map(|(parts, rec)| Admin1 {
            adm0_a3: text(&rec, "adm0_a3"),
            name: text(&rec, "name"),
            parts,
        })
        .collect())
}
