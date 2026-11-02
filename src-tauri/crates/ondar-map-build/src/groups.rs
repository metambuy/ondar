//! The country select's groups and names (0.1.2, F4), built from Natural Earth admin 0 v5.1.2
//! (attributes only) and CLDR 49 English, both pinned, into one committed table:
//! `ondar-stations/data/countries.tsv` (`code`, `group`, `name`). The app sorts and renders it;
//! nothing here runs in the app.
//!
//! ```text
//! cargo run -p ondar-map-build --release -- --countries ../ondar-stations/data/countries.tsv
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::ne::{self, Attrs};
use crate::{pins, tables};

pub const CLDR_TSV: &str = include_str!("../cldr.tsv");
pub const NAMES_TSV: &str = include_str!("../names.tsv");
pub const GROUP_OVERRIDES_TSV: &str = include_str!("../group-overrides.tsv");

/// A sovereign under this population is a small state (Martín, 2026-10-30; NE's estimates, 2019
/// for most).
pub const SMALL_UNDER: f64 = 200_000.0;
/// A territory of at least this population is in the main list (Martín, 2026-10-30 ~15:44: Hong
/// Kong, Puerto Rico, Macao, Réunion; Palestine by the same line).
pub const TERRITORY_MAIN_FROM: f64 = 500_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Main,
    Small,
    Territory,
}

impl Group {
    pub fn as_str(self) -> &'static str {
        match self {
            Group::Main => "main",
            Group::Small => "small",
            Group::Territory => "territory",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "main" => Some(Group::Main),
            "small" => Some(Group::Small),
            "territory" => Some(Group::Territory),
            _ => None,
        }
    }
}

/// The rule (ONDAR.md, "0.1.2: decisions", 7). A row is a territory if its sovereign is not
/// itself (`SOVEREIGNT ≠ ADMIN`), if NE types it `Dependency` or `Indeterminate`, or if it comes
/// from a map unit (an alias: an overseas department, Bonaire, Svalbard, the Australian islands);
/// a territory of [`TERRITORY_MAIN_FROM`] people or more is in the main list instead; otherwise
/// a row under [`SMALL_UNDER`] is a small state; otherwise main.
pub fn classify(a: &Attrs, from_map_unit: bool) -> Group {
    let territory = a.sovereignt != a.admin
        || a.kind == "Dependency"
        || a.kind == "Indeterminate"
        || from_map_unit;
    if territory {
        if a.pop_est >= TERRITORY_MAIN_FROM {
            Group::Main
        } else {
            Group::Territory
        }
    } else if a.pop_est < SMALL_UNDER {
        Group::Small
    } else {
        Group::Main
    }
}

/// One table row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub code: String,
    pub group: Group,
    pub name: String,
}

/// Every admin-0 code (`ISO_A2_EH`; on a code with several rows, the one whose `SOVEREIGNT` is its
/// `ADMIN`, which picks France over Clipperton, Australia over its three dependencies, measured in
/// the Step 0 h-report) and every alias (its map unit's attributes), with its group and CLDR
/// name, sorted by code.
pub fn rows(
    admin0: &[Attrs],
    map_units: &[Attrs],
    aliases: &[tables::Alias],
    cldr: &BTreeMap<String, String>,
    names: &[(String, String)],
    overrides: &[(String, Group)],
) -> Result<Vec<Row>, String> {
    let mut by_code: BTreeMap<String, (Attrs, bool)> = BTreeMap::new();
    for a in admin0 {
        let Some(code) = &a.code else { continue };
        let better = by_code
            .get(code)
            .is_none_or(|(held, _)| held.sovereignt != held.admin && a.sovereignt == a.admin);
        if better {
            by_code.insert(code.clone(), (a.clone(), false));
        }
    }
    for alias in aliases {
        let unit = map_units
            .iter()
            .find(|u| u.a3 == alias.gu_a3)
            .ok_or(format!("alias {}: no map unit {}", alias.code, alias.gu_a3))?;
        by_code.insert(alias.code.clone(), (unit.clone(), true));
    }
    let mut out = Vec::new();
    for (code, (attrs, from_map_unit)) in by_code {
        let group = overrides
            .iter()
            .find(|(c, _)| *c == code)
            .map_or_else(|| classify(&attrs, from_map_unit), |(_, g)| *g);
        let key = names
            .iter()
            .find(|(c, _)| *c == code)
            .map_or(code.as_str(), |(_, k)| k.as_str());
        let name = cldr
            .get(key)
            .ok_or(format!("{code}: CLDR has no name for key {key}"))?
            .clone();
        out.push(Row { code, group, name });
    }
    Ok(out)
}

/// The two-column tables (`names.tsv`, `group-overrides.tsv`): `#` lines and blanks skipped.
fn pairs(tsv: &str, file: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (n, line) in tsv.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [a, b] = f.as_slice() else {
            return Err(format!("{file}:{}: two fields expected", n + 1));
        };
        out.push(((*a).to_string(), (*b).to_string()));
    }
    Ok(out)
}

pub fn names(tsv: &str) -> Result<Vec<(String, String)>, String> {
    pairs(tsv, "names.tsv")
}

pub fn overrides(tsv: &str) -> Result<Vec<(String, Group)>, String> {
    pairs(tsv, "group-overrides.tsv")?
        .into_iter()
        .map(|(c, g)| {
            Group::parse(&g)
                .map(|g| (c.clone(), g))
                .ok_or(format!("group-overrides.tsv: {c}: unknown group {g}"))
        })
        .collect()
}

/// CLDR's `main.en.localeDisplayNames.territories` object, key → name.
pub fn cldr_names(json: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("CLDR: {e}"))?;
    let t = v
        .pointer("/main/en/localeDisplayNames/territories")
        .and_then(serde_json::Value::as_object)
        .ok_or("CLDR: no main.en.localeDisplayNames.territories")?;
    Ok(t.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect())
}

/// The committed file's text: a header naming the inputs and the rule, then one row per code.
pub fn table(rows: &[Row]) -> String {
    let mut t = String::from(
        "# GENERATED by `cargo run -p ondar-map-build --release -- --countries FILE` (0.1.2, F4);\n\
         # do not edit by hand. Inputs: Natural Earth admin 0 v5.1.2 (ondar-map-build/pins.tsv),\n\
         # CLDR 49.0.0 English territory names (ondar-map-build/cldr.tsv), names.tsv,\n\
         # group-overrides.tsv. Rule: ONDAR.md, \"0.1.2: decisions\", 7.\n\
         # code\tgroup\tname\n",
    );
    for r in rows {
        let _ = writeln!(t, "{}\t{}\t{}", r.code, r.group.as_str(), r.name);
    }
    t
}

/// The table from the pinned inputs in `input`.
pub fn build(input: &Path) -> Result<String, String> {
    let files = pins::check_all(input)?;
    let dbf = |n: &str| {
        files
            .get(n)
            .map(Vec::as_slice)
            .ok_or(format!("{n} missing"))
    };
    let admin0 = ne::attributes(dbf("ne_10m_admin_0_countries.dbf")?, "ADM0_A3")?;
    let map_units = ne::attributes(dbf("ne_10m_admin_0_map_units.dbf")?, "GU_A3")?;
    let mut cldr = None;
    for pin in pins::parse(CLDR_TSV)? {
        cldr = Some(pins::read_checked(&input.join(&pin.file), &pin)?);
    }
    let cldr = cldr_names(&cldr.ok_or("cldr.tsv pins no file")?)?;
    let rows = rows(
        &admin0,
        &map_units,
        &tables::aliases(tables::ALIASES_TSV)?,
        &cldr,
        &names(NAMES_TSV)?,
        &overrides(GROUP_OVERRIDES_TSV)?,
    )?;
    Ok(table(&rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(sovereignt: &str, admin: &str, kind: &str, pop_est: f64) -> Attrs {
        Attrs {
            code: None,
            a3: String::new(),
            sovereignt: sovereignt.into(),
            admin: admin.into(),
            kind: kind.into(),
            pop_est,
        }
    }

    /// The rule on the cases decided on 2026-10-30, with NE v5.1.2's own figures (the Step 0
    /// h-report's table). **Fails if** a clause of the rule is dropped or a line moves: without
    /// the map-unit clause Bonaire is a small state and Guadeloupe main; without the 500 000
    /// line Palestine and Réunion are territories; without `Indeterminate` Antarctica is small.
    #[test]
    fn the_rule_files_every_decided_case() {
        let cases = [
            (
                "PS",
                attrs("Israel", "Palestine", "Indeterminate", 4_685_306.0),
                false,
                Group::Main,
            ),
            (
                "XK",
                attrs("Kosovo", "Kosovo", "Disputed", 1_794_248.0),
                false,
                Group::Main,
            ),
            (
                "TW",
                attrs("Taiwan", "Taiwan", "Sovereign country", 23_568_378.0),
                false,
                Group::Main,
            ),
            (
                "HK",
                attrs("China", "Hong Kong S.A.R.", "Country", 7_507_400.0),
                false,
                Group::Main,
            ),
            (
                "MO",
                attrs("China", "Macao S.A.R", "Country", 640_445.0),
                false,
                Group::Main,
            ),
            (
                "PR",
                attrs(
                    "United States of America",
                    "Puerto Rico",
                    "Dependency",
                    3_193_694.0,
                ),
                false,
                Group::Main,
            ),
            (
                "RE",
                attrs("France", "France", "Geo unit", 859_959.0),
                true,
                Group::Main,
            ),
            (
                "GP",
                attrs("France", "France", "Geo unit", 395_700.0),
                true,
                Group::Territory,
            ),
            (
                "YT",
                attrs("France", "France", "Disputed", 279_471.0),
                true,
                Group::Territory,
            ),
            (
                "BQ",
                attrs("Netherlands", "Netherlands", "Geo unit", 25_987.0),
                true,
                Group::Territory,
            ),
            (
                "AQ",
                attrs("Antarctica", "Antarctica", "Indeterminate", 4_490.0),
                false,
                Group::Territory,
            ),
            (
                "CK",
                attrs("New Zealand", "Cook Islands", "Dependency", 17_459.0),
                false,
                Group::Territory,
            ),
            (
                "FK",
                attrs("United Kingdom", "Falkland Islands", "Disputed", 3_398.0),
                false,
                Group::Territory,
            ),
            (
                "GL",
                attrs("Denmark", "Greenland", "Country", 56_225.0),
                false,
                Group::Territory,
            ),
            (
                "LC",
                attrs("Saint Lucia", "Saint Lucia", "Sovereign country", 182_790.0),
                false,
                Group::Small,
            ),
            (
                "ST",
                attrs(
                    "São Tomé and Principe",
                    "São Tomé and Principe",
                    "Sovereign country",
                    215_056.0,
                ),
                false,
                Group::Main,
            ),
            (
                "VA",
                attrs("Vatican", "Vatican", "Sovereign country", 825.0),
                false,
                Group::Small,
            ),
        ];
        for (code, a, mu, want) in cases {
            assert_eq!(classify(&a, mu), want, "{code}");
        }
    }

    /// The lines are inclusive the way the decision reads: 200 000 is not "under 200 000", and a
    /// territory of exactly 500 000 is in the main list.
    #[test]
    fn the_lines_at_their_edges() {
        let sovereign = |p| classify(&attrs("A", "A", "Sovereign country", p), false);
        assert_eq!(sovereign(199_999.0), Group::Small);
        assert_eq!(sovereign(200_000.0), Group::Main);
        let territory = |p| classify(&attrs("B", "A", "Dependency", p), false);
        assert_eq!(territory(499_999.0), Group::Territory);
        assert_eq!(territory(500_000.0), Group::Main);
    }

    /// A code with several admin-0 rows takes the one whose sovereign is itself, whatever the
    /// order (France and Clipperton share FR, h-report §3).
    #[test]
    fn a_shared_code_takes_its_sovereign_row() {
        let fr = |admin: &str, pop| Attrs {
            code: Some("FR".into()),
            ..attrs("France", admin, "Country", pop)
        };
        let cldr: BTreeMap<String, String> = [("FR".to_string(), "France".to_string())].into();
        for rows0 in [
            vec![fr("Clipperton Island", 0.0), fr("France", 67_059_887.0)],
            vec![fr("France", 67_059_887.0), fr("Clipperton Island", 0.0)],
        ] {
            let got = rows(&rows0, &[], &[], &cldr, &[], &[]).unwrap();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].group, Group::Main, "{rows0:?}");
        }
    }

    /// An override wins over the rule; a name key from `names.tsv` replaces the code's own.
    #[test]
    fn overrides_and_name_keys_apply() {
        let ps = Attrs {
            code: Some("PS".into()),
            ..attrs("Israel", "Palestine", "Indeterminate", 4_685_306.0)
        };
        let cldr: BTreeMap<String, String> = [
            ("PS".to_string(), "Palestinian Territories".to_string()),
            ("PS-alt-short".to_string(), "Palestine".to_string()),
        ]
        .into();
        let names = [("PS".to_string(), "PS-alt-short".to_string())];
        let got = rows(std::slice::from_ref(&ps), &[], &[], &cldr, &names, &[]).unwrap();
        assert_eq!(got[0].name, "Palestine");
        let got = rows(
            &[ps],
            &[],
            &[],
            &cldr,
            &[],
            &[("PS".into(), Group::Territory)],
        )
        .unwrap();
        assert_eq!(
            (got[0].group, got[0].name.as_str()),
            (Group::Territory, "Palestinian Territories")
        );
    }

    /// The shipped tables parse, and the override table is empty: the rule gets every decided case
    /// right on its own (2026-11-02).
    #[test]
    fn the_shipped_tables_parse() {
        assert_eq!(names(NAMES_TSV).unwrap().len(), 4);
        assert_eq!(overrides(GROUP_OVERRIDES_TSV).unwrap(), []);
        assert_eq!(pins::parse(CLDR_TSV).unwrap().len(), 1);
    }

    /// Needs the NE and CLDR inputs (`scripts/fetch-natural-earth.sh`, `scripts/fetch-cldr.sh`).
    /// **What it pins:** the committed `ondar-stations/data/countries.tsv` is this tool's output
    /// on the pinned inputs, byte for byte, and two builds are identical. **Fails if** the table is
    /// edited by hand, or the rule, a table or an input changes without a rebuild.
    #[test]
    #[ignore = "needs the NE and CLDR inputs; cargo test -p ondar-map-build --release -- --ignored"]
    fn the_committed_table_is_the_build() {
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("input");
        let one = build(&input).unwrap();
        assert_eq!(one, build(&input).unwrap(), "two builds differ");
        let committed = include_str!("../../ondar-stations/data/countries.tsv");
        assert_eq!(one, committed, "rebuild with --countries");
    }
}
