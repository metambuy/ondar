//! The country select's groups and names (0.1.2, F4): `data/countries.tsv`, built by
//! `ondar-map-build --countries` from Natural Earth admin 0 v5.1.2 and CLDR 49 English, both
//! pinned there, and committed. Read once, here; nothing in it is fetched.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The table as committed.
pub const TABLE: &str = include_str!("../data/countries.tsv");

/// The three groups of the select, in their order on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// Sovereign states, and territories of 500 000 people or more.
    Main,
    /// Sovereign states under 200 000 people.
    Small,
    /// Everything else that is not a state of its own.
    Territory,
}

/// A code's group and its CLDR name, or `None` for a code the table does not know (one
/// radio-browser serves after the build).
pub fn lookup(code: &str) -> Option<(Group, &'static str)> {
    table().get(code).copied()
}

fn table() -> &'static HashMap<&'static str, (Group, &'static str)> {
    static T: OnceLock<HashMap<&'static str, (Group, &'static str)>> = OnceLock::new();
    T.get_or_init(|| parse(TABLE))
}

/// Rows are `code\tgroup\tname`; `#` lines are the header. A row that does not parse is skipped,
/// so the code falls back to radio-browser's name in the main group (`normalise`), never a panic;
/// the tests below refuse such a row in the committed file.
fn parse(tsv: &'static str) -> HashMap<&'static str, (Group, &'static str)> {
    tsv.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (code, group, name) = (f.next()?, f.next()?, f.next()?);
            let group = match group {
                "main" => Group::Main,
                "small" => Group::Small,
                "territory" => Group::Territory,
                _ => return None,
            };
            Some((code, (group, name)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the committed table parses: no row is silently skipped.
    #[test]
    fn every_row_parses() {
        let rows = TABLE.lines().filter(|l| !l.starts_with('#')).count();
        assert_eq!(table().len(), rows);
        assert_eq!(
            rows, 248,
            "the map's 248 codes (ondar-map-build's plan_all)"
        );
    }

    /// F4: every one of the 240 codes radio-browser serves (the census fixture, as `normalise`
    /// reads it) has a group and a name. **Fails if** the table misses a served code, which would
    /// then sit in the main list under radio-browser's own name ("The Holy See").
    #[test]
    fn every_served_code_has_a_group_and_a_name() {
        let served = crate::normalise::countries(include_bytes!("../fixtures/countries.json"))
            .expect("the fixture parses");
        assert_eq!(served.len(), 240);
        let missing: Vec<&str> = served
            .iter()
            .filter(|c| lookup(&c.code).is_none_or(|(_, name)| name.is_empty()))
            .map(|c| c.code.as_str())
            .collect();
        assert_eq!(missing, [] as [&str; 0]);
    }

    /// F4's decided cases (Martín, 2026-10-30), read from the committed table. **Fails if** a
    /// rebuild moves one: an input, rule or override change.
    #[test]
    fn the_decided_cases_are_in_their_groups() {
        for code in ["PS", "XK", "TW", "HK", "MO", "PR", "RE", "ST"] {
            assert_eq!(lookup(code).map(|t| t.0), Some(Group::Main), "{code}");
        }
        for code in [
            "GF", "GP", "MQ", "YT", "BQ", "AQ", "CK", "NU", "GL", "FK", "SJ", "AX", "TF",
        ] {
            assert_eq!(lookup(code).map(|t| t.0), Some(Group::Territory), "{code}");
        }
        for code in ["LC", "VA", "MC", "SM", "LI", "AD"] {
            assert_eq!(lookup(code).map(|t| t.0), Some(Group::Small), "{code}");
        }
    }

    /// The names: CLDR's, with the short form for the four (`names.tsv`), and no leading "The".
    #[test]
    fn the_names_are_cldrs() {
        for (code, name) in [
            ("PS", "Palestine"),
            ("HK", "Hong Kong"),
            ("MO", "Macao"),
            ("MM", "Myanmar"),
            ("VA", "Vatican City"),
            ("CI", "Côte d’Ivoire"),
            ("AX", "Åland Islands"),
        ] {
            assert_eq!(lookup(code).map(|t| t.1), Some(name), "{code}");
        }
        let the: Vec<&str> = table()
            .values()
            .map(|t| t.1)
            .filter(|n| n.starts_with("The "))
            .collect();
        assert_eq!(the, [] as [&str; 0]);
    }
}
