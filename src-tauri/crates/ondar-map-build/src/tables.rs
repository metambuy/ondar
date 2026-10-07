//! The tool's hand-checked inputs (D5): `overrides.tsv` (S2), `insets.tsv` (S6), `aliases.tsv`
//! (R7 / S4). Parsed strictly: a malformed row is an error naming its line.

use ondar_map::format::Corner;

pub const OVERRIDES_TSV: &str = include_str!("../overrides.tsv");
pub const INSETS_TSV: &str = include_str!("../insets.tsv");
pub const ALIASES_TSV: &str = include_str!("../aliases.tsv");

#[derive(Clone, Debug, PartialEq)]
pub struct Override {
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsetRow {
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub keep: String,
    pub corner: Corner,
    pub rect: [f64; 4],
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alias {
    pub code: String,
    pub gu_a3: String,
    pub parent_a3: String,
}

fn rows(name: &str, tsv: &str, fields: usize) -> Result<Vec<(usize, Vec<String>)>, String> {
    let mut out = Vec::new();
    for (n, line) in tsv.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<String> = line.split('\t').map(str::to_string).collect();
        if f.len() != fields {
            return Err(format!(
                "{name}:{}: {fields} fields expected, {}",
                n + 1,
                f.len()
            ));
        }
        out.push((n + 1, f));
    }
    Ok(out)
}

fn num(name: &str, line: usize, s: &str) -> Result<f64, String> {
    s.trim()
        .parse()
        .map_err(|e| format!("{name}:{line}: {s:?}: {e}"))
}

fn field(f: &[String], i: usize) -> &str {
    f.get(i).map(String::as_str).unwrap_or_default()
}

pub fn overrides(tsv: &str) -> Result<Vec<Override>, String> {
    rows("overrides.tsv", tsv, 4)?
        .into_iter()
        .map(|(n, f)| {
            Ok(Override {
                code: field(&f, 0).to_string(),
                lat: num("overrides.tsv", n, field(&f, 1))?,
                lon: num("overrides.tsv", n, field(&f, 2))?,
                reason: field(&f, 3).to_string(),
            })
        })
        .collect()
}

pub fn insets(tsv: &str) -> Result<Vec<InsetRow>, String> {
    let name = "insets.tsv";
    rows(name, tsv, 10)?
        .into_iter()
        .map(|(n, f)| {
            let corner = match field(&f, 4) {
                "top-left" => Corner::TopLeft,
                "top-right" => Corner::TopRight,
                "bottom-left" => Corner::BottomLeft,
                "bottom-right" => Corner::BottomRight,
                other => return Err(format!("{name}:{n}: corner {other:?}")),
            };
            Ok(InsetRow {
                code: field(&f, 0).to_string(),
                lat: num(name, n, field(&f, 1))?,
                lon: num(name, n, field(&f, 2))?,
                keep: field(&f, 3).to_string(),
                corner,
                rect: [
                    num(name, n, field(&f, 5))?,
                    num(name, n, field(&f, 6))?,
                    num(name, n, field(&f, 7))?,
                    num(name, n, field(&f, 8))?,
                ],
                label: field(&f, 9).to_string(),
            })
        })
        .collect()
}

pub fn aliases(tsv: &str) -> Result<Vec<Alias>, String> {
    Ok(rows("aliases.tsv", tsv, 3)?
        .into_iter()
        .map(|(_, f)| Alias {
            code: field(&f, 0).to_string(),
            gu_a3: field(&f, 1).to_string(),
            parent_a3: field(&f, 2).to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped `overrides.tsv` (1), `insets.tsv` (14) and `aliases.tsv` (9) parse.
    #[test]
    fn the_shipped_tables_parse() {
        assert_eq!(overrides(OVERRIDES_TSV).unwrap().len(), 1);
        let i = insets(INSETS_TSV).unwrap();
        assert_eq!(i.len(), 14);
        assert_eq!(i[0].label, "Azores");
        assert_eq!(i[0].rect, [10.0, 24.0, 92.0, 52.0]);
        let a = aliases(ALIASES_TSV).unwrap();
        assert_eq!(a.len(), 9);
        assert!(a.iter().all(|x| x.code.len() == 2 && x.gu_a3.len() == 3));
    }

    /// A malformed row names its line.
    #[test]
    fn a_malformed_row_names_its_line() {
        let e = insets("PT\t1\t2\tk\tleft\t0\t0\t1\t1\tX\n").unwrap_err();
        assert!(e.contains("insets.tsv:1"), "{e}");
        let e = overrides("# c\nMY\t4.04\n").unwrap_err();
        assert!(e.contains("overrides.tsv:2"), "{e}");
    }
}
