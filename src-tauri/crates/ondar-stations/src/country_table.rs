//! The country select's groups and names (0.1.2, F4): `data/countries.tsv`, built by
//! `ondar-map-build --countries` from Natural Earth admin 0 v5.1.2 and CLDR 49 English, both
//! pinned there, and committed. Read once, here; nothing in it is fetched.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::OnceLock;

use icu_normalizer::DecomposingNormalizerBorrowed;

use crate::model::{Country, CountryGroup as Group};

/// The table as committed.
pub const TABLE: &str = include_str!("../data/countries.tsv");

/// A code's group and its CLDR name, or `None` for a code the table does not know (one
/// radio-browser serves after the build).
pub fn lookup(code: &str) -> Option<(Group, &'static str)> {
    table().get(code).copied()
}

/// The list as the page shows it (0.1.2, F4): each country's group and CLDR name from the table,
/// then sorted by group (main, small states, territories), and inside each by [`sort_key`], the
/// name with its accents folded ("Åland Islands" among the A's, "Réunion" among the R's). A code
/// the table does not know keeps radio-browser's name, in the main group, and is logged once per
/// call at INFO (radio-browser's data, not a fault; as the `XX` drop line).
pub fn arrange(mut items: Vec<Country>) -> Vec<Country> {
    let mut unknown = Vec::new();
    for c in &mut items {
        if let Some((group, name)) = lookup(&c.code) {
            c.group = group;
            name.clone_into(&mut c.name);
        } else {
            c.group = Group::Main;
            unknown.push(c.code.clone());
        }
    }
    // A row still nameless here came from a cache stored before `normalise` dropped such rows
    // (0.1.2: `UN`); it would be a blank option.
    let nameless: Vec<String> = items
        .iter()
        .filter(|c| c.name.trim().is_empty())
        .map(|c| c.code.clone())
        .collect();
    items.retain(|c| !c.name.trim().is_empty());
    unknown.retain(|code| !nameless.contains(code));
    if !unknown.is_empty() {
        log::info!(
            "countries: {} code(s) not in the built table, listed under radio-browser's names: \
             {unknown:?}",
            unknown.len()
        );
    }
    if !nameless.is_empty() {
        log::info!(
            "countries: dropped {} cached code(s) with no name: {nameless:?}",
            nameless.len()
        );
    }
    items.sort_by(order);
    items
}

fn order(a: &Country, b: &Country) -> Ordering {
    a.group
        .cmp(&b.group)
        .then_with(|| sort_key(&a.name).cmp(&sort_key(&b.name)))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.code.cmp(&b.code))
}

/// A name's sort key: canonical decomposition (NFD, `icu_normalizer` 2.3.0, already in the tree
/// through `url`), the combining diacritical marks it splits off (U+0300–U+036F, which is where
/// NFD puts every accent of a Latin letter) dropped, then lower case. Not a collation: A to Z for
/// about 250 English names, where "Å" sorts as "a" and "ç" as "c" (ONDAR.md, "0.1.2: decisions",
/// the buy-before-build table: `icu_collator` rejected for its data).
pub fn sort_key(name: &str) -> String {
    DecomposingNormalizerBorrowed::new_nfd()
        .normalize(name)
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
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

    fn served() -> Vec<Country> {
        crate::normalise::countries(include_bytes!("../fixtures/countries.json"))
            .expect("the fixture parses")
    }

    fn position(list: &[Country], code: &str) -> usize {
        list.iter()
            .position(|c| c.code == code)
            .unwrap_or_else(|| panic!("{code} not listed"))
    }

    /// F4's order, on the served fixture: the three groups in turn, A to Z inside each, accents
    /// folded. **Fails on** the served order (by ISO code: Andorra, The United Arab Emirates,
    /// Afghanistan, ...; `cache.rs`'s `ORDER BY rowid`, 0.1.1), on a sort that ignores the groups,
    /// and on a byte-wise sort, which puts "Åland Islands" after "Zimbabwe" and "Réunion" after
    /// "Rwanda"; the plant `_handover/v012/plants/f4-sort-bytewise.patch` fails it.
    #[test]
    fn the_list_is_grouped_then_a_to_z_with_accents_folded() {
        let list = arrange(served());
        let groups: Vec<Group> = list.iter().map(|c| c.group).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted, "the groups come in turn");
        assert_eq!(list[0].name, "Afghanistan");
        for (before, after) in [
            ("AL", "DZ"), // Albania, Algeria
            ("CR", "CI"), // Costa Rica, Côte d’Ivoire
            ("CI", "HR"), // Côte d’Ivoire, Croatia
            ("CU", "CY"), // Cuba, Cyprus
            ("QA", "RE"), // Qatar, Réunion
            ("RE", "RO"), // Réunion, Romania
            ("SM", "ST"), // San Marino is small; São Tomé & Príncipe is main: groups first
            ("AX", "GG"), // Åland Islands, Guernsey (territories)
        ] {
            let (b, a) = (position(&list, before), position(&list, after));
            if list[b].group == list[a].group {
                assert!(b < a, "{} before {}", list[b].name, list[a].name);
            }
        }
        let main_st = position(&list, "ST");
        let small_sm = position(&list, "SM");
        assert!(main_st < small_sm, "the main list before the small states");
    }

    /// The sort key folds what CLDR's English names carry.
    #[test]
    fn the_sort_key_folds_accents_and_case() {
        assert_eq!(sort_key("Åland Islands"), "aland islands");
        assert_eq!(sort_key("Côte d’Ivoire"), "cote d’ivoire");
        assert_eq!(sort_key("São Tomé & Príncipe"), "sao tome & principe");
        assert_eq!(sort_key("Réunion"), "reunion");
        assert_eq!(sort_key("Türkiye"), "turkiye");
    }

    /// A cached row with no name (stored by 0.1.1, before `normalise` dropped them: `UN`) is not
    /// listed. **Fails if** it reaches the page as a blank option (Martín's by-eye check of
    /// commit 8: one above Afghanistan).
    #[test]
    fn a_cached_row_with_no_name_is_not_listed() {
        let list = arrange(vec![
            Country {
                code: "UN".into(),
                name: String::new(),
                group: Group::Main,
                station_count: 1,
            },
            Country {
                code: "AF".into(),
                name: "Afghanistan".into(),
                group: Group::Main,
                station_count: 2,
            },
        ]);
        assert_eq!(
            list.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(),
            ["AF"]
        );
    }

    /// A code the table does not know: radio-browser's name, the main group, still sorted.
    #[test]
    fn an_unknown_code_keeps_its_name_in_the_main_group() {
        let list = arrange(vec![
            Country {
                code: "QZ".into(),
                name: "Zeta Land".into(),
                group: Group::Territory,
                station_count: 1,
            },
            Country {
                code: "PT".into(),
                name: "Portugal (radio-browser)".into(),
                group: Group::Main,
                station_count: 371,
            },
        ]);
        assert_eq!(
            list.iter()
                .map(|c| (c.code.as_str(), c.name.as_str(), c.group))
                .collect::<Vec<_>>(),
            [
                ("PT", "Portugal", Group::Main),
                ("QZ", "Zeta Land", Group::Main)
            ]
        );
    }

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
