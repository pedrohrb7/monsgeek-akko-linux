//! Physical key classification and the `--keys` selector.
//!
//! Classes are derived from the matrix position names rather than hand-listed, so
//! they stay correct if the name table grows. The seven content classes partition
//! the named positions exactly — see `classes_partition_named_positions`.
//!
//! This lives in `iot_driver` rather than `monsgeek-transport` because the
//! modifier rule consults the HID table (`crate::protocol::hid`); the transport
//! crate stays wire-only.

use crate::keymap::{KeyTable, default_keycode};
use monsgeek_transport::protocol::{MatrixPos, matrix};
use std::fmt;
use std::str::FromStr;

/// Matrix positions are column-major with six rows per column.
const ROWS: u8 = 6;

/// A group of physical keys selectable by name on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    /// Every named position.
    All,
    Alpha,
    Digit,
    /// `Alpha ∪ Digit`.
    Alnum,
    Function,
    Modifier,
    Navigation,
    Punctuation,
    /// Named keys in none of the above: Esc, Tab, Caps, Spc, Del, Bksp, Ent.
    Special,
    /// One physical row of the matrix (`index % 6`). Row 0 is the F-row.
    Row(u8),
}

impl KeyClass {
    /// Every class except the per-row ones, in listing order.
    pub const CONTENT: [KeyClass; 7] = [
        Self::Alpha,
        Self::Digit,
        Self::Function,
        Self::Modifier,
        Self::Navigation,
        Self::Punctuation,
        Self::Special,
    ];

    /// Classes offered as cycleable filter states (TUI) and in error messages.
    pub const ALL: [KeyClass; 9] = [
        Self::All,
        Self::Alpha,
        Self::Digit,
        Self::Alnum,
        Self::Function,
        Self::Modifier,
        Self::Navigation,
        Self::Punctuation,
        Self::Special,
    ];

    pub fn label(self) -> String {
        match self {
            Self::All => "All".into(),
            Self::Alpha => "Alpha".into(),
            Self::Digit => "Digit".into(),
            Self::Alnum => "Alnum".into(),
            Self::Function => "Function".into(),
            Self::Modifier => "Modifier".into(),
            Self::Navigation => "Nav".into(),
            Self::Punctuation => "Punct".into(),
            Self::Special => "Special".into(),
            Self::Row(n) => format!("Row{n}"),
        }
    }

    /// Whether `index` belongs to this class.
    pub fn contains(self, index: MatrixPos) -> bool {
        let name = matrix::key_name(index);
        if name == "?" {
            return false;
        }
        match self {
            Self::All => true,
            Self::Alnum => Self::Alpha.contains(index) || Self::Digit.contains(index),
            Self::Row(n) => index.row() == n,
            _ => content_class(index) == Some(self),
        }
    }

    /// Every matrix position in this class, in layout (reading) order.
    pub fn members(self) -> Vec<MatrixPos> {
        let mut v: Vec<MatrixPos> = (0..matrix::KEY_COUNT)
            .map(MatrixPos::new)
            .filter(|&i| self.contains(i))
            .collect();
        v.sort_by_key(|&i| (i.row(), i.col()));
        v
    }
}

impl fmt::Display for KeyClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for KeyClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        if let Some(n) = lower.strip_prefix("row").and_then(|r| r.parse::<u8>().ok())
            && n < ROWS
        {
            return Ok(Self::Row(n));
        }
        match lower.as_str() {
            "all" => Ok(Self::All),
            "alpha" | "letter" | "letters" => Ok(Self::Alpha),
            "digit" | "digits" | "num" | "number" => Ok(Self::Digit),
            "alnum" | "alphanumeric" => Ok(Self::Alnum),
            "function" | "fkey" | "fkeys" | "f" => Ok(Self::Function),
            "modifier" | "modifiers" | "mod" | "mods" => Ok(Self::Modifier),
            "nav" | "navigation" | "arrows" => Ok(Self::Navigation),
            "punct" | "punctuation" | "symbol" | "symbols" | "sym" => Ok(Self::Punctuation),
            "special" => Ok(Self::Special),
            _ => Err(format!("unknown key class: \"{s}\"")),
        }
    }
}

/// The single content class a position belongs to, or `None` for unnamed slots.
///
/// The seven content classes are mutually exclusive and cover every named
/// position, so this is a total function over the named matrix.
pub fn content_class(index: MatrixPos) -> Option<KeyClass> {
    let name = matrix::key_name(index);
    if name == "?" {
        return None;
    }
    // Modifiers first: the HID table already knows which positions default to a
    // modifier usage, so only Fn — which has no HID code — needs naming.
    if name == "Fn" || default_keycode(index).is_modifier() {
        return Some(KeyClass::Modifier);
    }
    if NAVIGATION.contains(&name) {
        return Some(KeyClass::Navigation);
    }
    // F1..F12. Guarded on the digits so a future "Fn"-like name can't slip in.
    if let Some(rest) = name.strip_prefix('F')
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(KeyClass::Function);
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(KeyClass::Alpha);
        }
        if c.is_ascii_digit() {
            return Some(KeyClass::Digit);
        }
        return Some(KeyClass::Punctuation);
    }
    // The ISO extras are punctuation despite their multi-character labels.
    if name == "IntlRo" || name == "IntlBs" {
        return Some(KeyClass::Punctuation);
    }
    Some(KeyClass::Special)
}

const NAVIGATION: [&str; 8] = ["Left", "Right", "Up", "Down", "Home", "End", "PgUp", "PgDn"];

/// Names for keys whose matrix label is punctuation, so they can be typed on a
/// command line where `,` is the list separator and `-` would read as a flag.
const PUNCT_ALIASES: [(&str, &str); 11] = [
    ("comma", ","),
    ("period", "."),
    ("dot", "."),
    ("slash", "/"),
    ("backslash", "\\"),
    ("minus", "-"),
    ("dash", "-"),
    ("equal", "="),
    ("grave", "`"),
    ("backtick", "`"),
    ("semicolon", ";"),
];

/// One `--keys` term: a class, a key name, an index (`#9`), or an index range
/// (`9..14`), optionally negated with `!`.
///
/// A bare number is a *key name* — `9` is the digit key, not position 9 — because
/// names and classes are the vocabulary this selector is for. Positions need `#`,
/// which also keeps `9` and `#9` from quietly meaning different keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySelector {
    pub negated: bool,
    term: Term,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    Class(KeyClass),
    Index(MatrixPos),
    Range(u8, u8),
    /// A key name, resolved against the board's [`KeyTable`] once it is known:
    /// the same name sits at different positions on different layouts.
    Name(String),
}

impl KeySelector {
    /// Matrix positions this term selects.
    fn members(&self, table: &KeyTable) -> Result<Vec<MatrixPos>, String> {
        Ok(match &self.term {
            Term::Class(c) => c.members(),
            Term::Index(i) => vec![*i],
            Term::Range(a, b) => (*a..=*b).map(MatrixPos::new).collect(),
            Term::Name(name) => vec![table.resolve_name(name).map_err(|_| unknown_term(name))?],
        })
    }

    /// Resolve a whole selector list: the union of the positive terms (or every
    /// named position when there are none) minus the union of the negated ones.
    pub fn resolve(selectors: &[KeySelector], table: &KeyTable) -> Result<Vec<MatrixPos>, String> {
        let mut keys: Vec<MatrixPos> = Vec::new();
        let mut any_positive = false;
        for s in selectors.iter().filter(|s| !s.negated) {
            any_positive = true;
            keys.extend(s.members(table)?);
        }
        if !any_positive {
            keys = KeyClass::All.members();
        }
        for excluded in selectors.iter().filter(|s| s.negated) {
            let drop = excluded.members(table)?;
            keys.retain(|k| !drop.contains(k));
        }
        keys.sort_by_key(|&i| (i.row(), i.col()));
        keys.dedup();
        Ok(keys)
    }
}

fn unknown_term(body: &str) -> String {
    format!(
        "unknown key or class: \"{body}\". Classes: {}; or a key name, #index, or N..M range",
        KeyClass::ALL
            .iter()
            .map(|c| c.label().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

impl FromStr for KeySelector {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // `!` rather than `-`, which is itself a key name.
        let (negated, body) = match s.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, s),
        };
        if body.is_empty() {
            return Err("empty key selector".to_string());
        }

        // `..` rather than `-`, for the same reason.
        if let Some((a, b)) = body.split_once("..") {
            let parse = |v: &str, which| {
                v.trim()
                    .parse::<u8>()
                    .map_err(|_| format!("{which} of range \"{body}\" is not an index"))
            };
            let (a, b) = (parse(a, "start")?, parse(b, "end")?);
            if a > b {
                return Err(format!("range \"{body}\" is inverted"));
            }
            return Ok(Self {
                negated,
                term: Term::Range(a, b),
            });
        }

        // `#9` is a position; a bare `9` is the key labelled "9".
        if let Some(digits) = body.strip_prefix('#') {
            let index = digits
                .trim()
                .parse::<u8>()
                .map_err(|_| format!("\"{body}\" is not a matrix position"))?;
            return Ok(Self {
                negated,
                term: Term::Index(MatrixPos::new(index)),
            });
        }

        if let Ok(class) = body.parse::<KeyClass>() {
            return Ok(Self {
                negated,
                term: Term::Class(class),
            });
        }
        let aliased = PUNCT_ALIASES
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(body))
            .map(|&(_, name)| name)
            .unwrap_or(body);
        Ok(Self {
            negated,
            term: Term::Name(aliased.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monsgeek_transport::protocol::HidUsage;

    /// The seven content classes are disjoint and cover every named position. If a
    /// name is added to the matrix table without a home, this fails.
    #[test]
    fn classes_partition_named_positions() {
        let named: Vec<MatrixPos> = (0..matrix::KEY_COUNT)
            .map(MatrixPos::new)
            .filter(|&i| matrix::key_name(i) != "?")
            .collect();
        let mut total = 0;
        for class in KeyClass::CONTENT {
            let members = class.members();
            for &i in &members {
                assert_eq!(
                    content_class(i),
                    Some(class),
                    "{} ({}) claimed by two classes",
                    matrix::key_name(i),
                    i.get()
                );
            }
            total += members.len();
        }
        assert_eq!(total, named.len(), "classes do not cover every named key");
        assert_eq!(named.len(), 84, "matrix name table changed size");
    }

    #[test]
    fn class_sizes_match_the_physical_board() {
        assert_eq!(KeyClass::Alpha.members().len(), 26);
        assert_eq!(KeyClass::Digit.members().len(), 10);
        assert_eq!(KeyClass::Alnum.members().len(), 36);
        assert_eq!(KeyClass::Function.members().len(), 12);
        assert_eq!(KeyClass::Modifier.members().len(), 8);
        assert_eq!(KeyClass::Navigation.members().len(), 8);
        assert_eq!(KeyClass::Punctuation.members().len(), 13);
        assert_eq!(KeyClass::Special.members().len(), 7);
        assert_eq!(KeyClass::All.members().len(), 84);
    }

    /// `Fn` has no HID code, so it can only reach Modifier by being named.
    #[test]
    fn fn_key_is_a_modifier() {
        let fn_index = matrix::key_index_from_name("Fn").unwrap();
        assert_eq!(content_class(fn_index), Some(KeyClass::Modifier));
    }

    fn resolve(spec: &str) -> Vec<MatrixPos> {
        let sels: Vec<KeySelector> = spec
            .split(',')
            .map(|t| t.parse().unwrap_or_else(|e| panic!("{t}: {e}")))
            .collect();
        KeySelector::resolve(&sels, &KeyTable::generic()).unwrap_or_else(|e| panic!("{spec}: {e}"))
    }

    #[test]
    fn selector_resolves_classes_names_and_negation() {
        assert_eq!(resolve("alpha").len(), 26);
        assert_eq!(resolve("w,a,s,d").len(), 4);
        assert_eq!(resolve("alpha,!w").len(), 25);
        assert_eq!(resolve("all,!modifier").len(), 84 - 8);
        // Classes and individual keys mix; duplicates collapse.
        assert_eq!(resolve("alpha,function,w,a,s,d").len(), 26 + 12);
    }

    /// A bare number is the key with that label; `#n` is the matrix position.
    #[test]
    fn bare_number_is_a_key_name_and_hash_is_a_position() {
        assert_eq!(
            resolve("9"),
            vec![matrix::key_index_from_name("9").unwrap()]
        );
        assert_eq!(resolve("#9"), vec![MatrixPos::new(9)]);
        assert_ne!(resolve("9"), resolve("#9"));
    }

    #[test]
    fn ranges_are_positions() {
        assert_eq!(resolve("9..14").len(), 6);
    }

    /// An empty list means "every key", so a lone negation subtracts from all.
    #[test]
    fn negation_alone_starts_from_every_key() {
        assert_eq!(resolve("!alpha").len(), 84 - 26);
    }

    /// `,` is the list delimiter and `-` reads as a flag, so those keys need names.
    #[test]
    fn punctuation_aliases_reach_otherwise_untypeable_keys() {
        assert_eq!(
            resolve("comma"),
            vec![matrix::key_index_from_name(",").unwrap()]
        );
        assert_eq!(
            resolve("dash"),
            vec![matrix::key_index_from_name("-").unwrap()]
        );
        assert_eq!(
            resolve("slash"),
            vec![matrix::key_index_from_name("/").unwrap()]
        );
    }

    #[test]
    fn unknown_token_lists_the_classes() {
        let sel: KeySelector = "nonsense".parse().unwrap();
        let err = KeySelector::resolve(&[sel], &KeyTable::generic()).unwrap_err();
        assert!(err.contains("alpha"), "{err}");
        assert!(err.contains("function"), "{err}");
    }

    /// Names go through the board's own table, not the generic layout.
    #[test]
    fn names_resolve_against_the_board_table() {
        let mut names = vec![String::new(); 126];
        let mut defaults = vec![HidUsage::NONE; 126];
        // The FUN60 PRO's RCtrl sits where the generic table has "Down"; its
        // generic "RCtl" (71) is the board's Fn key.
        names[83] = "RCtrl".into();
        defaults[83] = HidUsage::new(0xE4);
        let table = KeyTable::new(names, defaults);
        let sel: KeySelector = "rctrl".parse().unwrap();
        assert_eq!(
            KeySelector::resolve(&[sel], &table).unwrap(),
            vec![MatrixPos::new(83)]
        );
    }

    #[test]
    fn rows_are_selectable() {
        // Row 0 is the F-row: F1..F12 plus Esc and Del.
        let row0 = KeyClass::Row(0).members();
        assert!(row0.iter().all(|&i| i.row() == 0));
        assert!(row0.contains(&matrix::key_index_from_name("F1").unwrap()));
    }
}
