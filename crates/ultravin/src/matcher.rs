//! Key matching: the plain SQL-`LIKE` branch and the bracket-class branch.
//!
//! `var_keys` (e.g. `CM826|3A004352`) is matched against a pattern's `keys`.
//! Plain keys use `LIKE replace(keys,'*','_') || '%'`; bracket keys use the
//! Postgres `~` regex produced by [`sqlwild_to_regex`] (a port of the SQL of
//! the same name, since the stored `keys_regex` column is absent from the dump).
//!
//! Those bracket regexes are not general regexes: `sqlwild_to_regex` only ever
//! emits `^<body>.*`, where every body token consumes exactly one character —
//! a literal, an escaped literal (`\X`), a `.` (any char), or a positive class
//! `[...]`. So a match is just an anchored, fixed-length prefix check, which a
//! tiny token matcher does without the lazy-DFA machinery (~8% of decode time).
//! Anything the parser doesn't fully recognise falls back to the real `regex`
//! engine, so behaviour is identical to compiling the pattern fresh every call.

use regex::Regex;

use crate::hash::IntMap;
use crate::{db::Db, tables::ArchivedPattern};

/// Match each distinct key once, then expand it to its eligible pattern rows.
/// Owned by its database, so string ids cannot alias another loaded artifact.
pub(crate) struct PatternIndex {
    groups: Vec<KeyGroup>,
    literals: IntMap<u16, Vec<usize>>,
    positions: Vec<usize>,
    fallback: Vec<usize>,
    pub(crate) formula_rows: Vec<u32>,
}

struct KeyGroup {
    key: u32,
    matcher: Option<Matcher>,
    rows: Vec<u32>,
}

impl PatternIndex {
    pub(crate) fn build(db: &Db, start: u32, patterns: &[ArchivedPattern]) -> Self {
        let mut groups: Vec<KeyGroup> = Vec::new();
        let mut by_key: IntMap<u64, usize> = IntMap::default();
        let eligible = db.pattern_element_ok();
        let mut formula_rows = Vec::new();
        for (i, p) in patterns.iter().enumerate() {
            if db.s(p.keys.to_native()).contains('#') {
                formula_rows.push(start + i as u32);
            }
            if !eligible
                .get(p.elementid.to_native() as usize)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let key = if p.has_bracket {
                p.keys_regex.to_native()
            } else {
                p.keys.to_native()
            };
            let identity = u64::from(key) * 2 + u64::from(p.has_bracket);
            let group = *by_key.entry(identity).or_insert_with(|| {
                let index = groups.len();
                groups.push(KeyGroup {
                    key,
                    matcher: p.has_bracket.then(|| Matcher::compile(db.s(key))),
                    rows: Vec::new(),
                });
                index
            });
            groups[group].rows.push(start + i as u32);
        }
        let mut literals: IntMap<u16, Vec<usize>> = IntMap::default();
        let mut positions = Vec::new();
        let mut fallback = Vec::new();
        for (i, group) in groups.iter().enumerate() {
            let literal = match &group.matcher {
                Some(Matcher::Sets(sets)) => sets.iter().enumerate().find_map(|(pos, set)| {
                    if set.iter().map(|word| word.count_ones()).sum::<u32>() != 1 {
                        return None;
                    }
                    let word = set.iter().position(|word| *word != 0).unwrap();
                    Some((pos, (word * 64 + set[word].trailing_zeros() as usize) as u8))
                }),
                Some(Matcher::Fallback(_)) => None,
                None => db
                    .s(group.key)
                    .bytes()
                    .enumerate()
                    .find(|(_, b)| !matches!(b, b'*' | b'_')),
            };
            // Normalized VIN keys have at most 14 bytes. Longer or unusual
            // patterns keep the ordinary matcher as the authority.
            if let Some((pos, byte)) = literal.filter(|(pos, _)| *pos < 14) {
                literals
                    .entry((pos as u16) * 256 + u16::from(byte))
                    .or_default()
                    .push(i);
                positions.push(pos);
            } else {
                fallback.push(i);
            }
        }
        positions.sort_unstable();
        positions.dedup();
        Self {
            groups,
            literals,
            positions,
            fallback,
            formula_rows,
        }
    }

    pub(crate) fn hits(&self, db: &Db, keys: &str) -> Vec<u32> {
        let mut hits = Vec::new();
        let candidates = self
            .positions
            .iter()
            .filter_map(|&pos| {
                let byte = *keys.as_bytes().get(pos)?;
                self.literals.get(&((pos as u16) * 256 + u16::from(byte)))
            })
            .flatten()
            .chain(&self.fallback);
        for &i in candidates {
            let group = &self.groups[i];
            let matched = match &group.matcher {
                Some(matcher) => matcher.is_match(keys),
                None => like_match(keys.as_bytes(), db.s(group.key).as_bytes()),
            };
            if matched {
                hits.extend_from_slice(&group.rows);
            }
        }
        hits
    }
}

/// Port of `vpic.sqlwild_to_regex`: turn a wildcard key into an anchored regex.
pub fn sqlwild_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    for ch in pattern.chars() {
        match ch {
            '*' => out.push('.'),
            '[' | ']' => out.push(ch),
            '|' => out.push_str("\\|"),
            '\\' | '.' | '^' | '$' | '+' | '?' | '{' | '}' | '(' | ')' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    let out = out.replace("1-A", "1A");
    format!("^{out}.*")
}

/// SQL `var_keys LIKE replace(keys,'*','_') || '%'` for the plain (no-bracket)
/// branch: `*`/`_` match any single char, everything else is literal, and a
/// trailing `%` leaves the remainder of `var_keys` unconstrained.
pub fn like_match(var_keys: &[u8], keys: &[u8]) -> bool {
    if var_keys.len() < keys.len() {
        return false;
    }
    // `var_keys` is the longer/equal slice, so `zip` yields exactly `keys.len()`
    // pairs and the indexing bounds checks fall away.
    for (&v, &k) in var_keys.iter().zip(keys) {
        if k != b'*' && k != b'_' && v != k {
            return false;
        }
    }
    true
}

/// One token of a parsed bracket regex body; each matches exactly one byte.
enum Token {
    /// `.` — any byte (regex `.` excludes `\n`; honoured for exactness).
    Any,
    /// A literal byte (a plain char or an escaped `\X`).
    Lit(u8),
    /// A positive `[...]` class as inclusive `(lo, hi)` byte ranges (a single
    /// char is `(c, c)`).
    Class(Vec<(u8, u8)>),
}

impl Token {
    /// The token as a 256-bit "allowed byte" set — the form the matcher runs on.
    fn byte_set(&self) -> ByteSet {
        let mut set = [0u64; 4];
        let mut allow = |b: u8| set[(b >> 6) as usize] |= 1 << (b & 63);
        match self {
            // regex `.` excludes `\n`; honoured for exactness.
            Token::Any => (0..=u8::MAX).filter(|&b| b != b'\n').for_each(&mut allow),
            Token::Lit(c) => allow(*c),
            Token::Class(ranges) => ranges
                .iter()
                .flat_map(|&(lo, hi)| lo..=hi)
                .for_each(&mut allow),
        }
        set
    }
}

/// One key position's allowed bytes, one bit per byte value.
///
/// The bracket keys are 21% of the archive's patterns but the scan tests every
/// one of them on every decode, so the shape of this matters: a flat array of
/// these is one contiguous read per pattern, where the `Token` list it is built
/// from was a `Vec` of enums whose classes each owned another heap `Vec` — a
/// pointer chase per key position.
type ByteSet = [u64; 4];

#[inline]
fn set_contains(set: &ByteSet, b: u8) -> bool {
    set[(b >> 6) as usize] >> (b & 63) & 1 == 1
}

/// A compiled bracket matcher: the fast fixed-length-prefix token path, or the
/// real `regex` engine for anything the parser doesn't fully recognise (`None`
/// preserves the old "compile error => never match" behaviour).
enum Matcher {
    /// One allowed-byte set per key position, in one contiguous allocation.
    Sets(Box<[ByteSet]>),
    Fallback(Option<Regex>),
}

impl Matcher {
    fn compile(regex: &str) -> Matcher {
        match parse_tokens(regex) {
            Some(tokens) => Matcher::Sets(tokens.iter().map(Token::byte_set).collect()),
            None => Matcher::Fallback(Regex::new(regex).ok()),
        }
    }

    #[inline]
    fn is_match(&self, haystack: &str) -> bool {
        match self {
            Matcher::Sets(sets) => {
                let b = haystack.as_bytes();
                if b.len() < sets.len() {
                    return false;
                }
                sets.iter().zip(b).all(|(s, &c)| set_contains(s, c))
            }
            Matcher::Fallback(re) => re.as_ref().is_some_and(|r| r.is_match(haystack)),
        }
    }
}

/// Parse a `sqlwild_to_regex` output (`^<body>.*`) into single-byte tokens, or
/// `None` if it contains anything outside the expected grammar (then the caller
/// falls back to the real regex engine). ASCII-only by construction (VIN keys).
fn parse_tokens(regex: &str) -> Option<Vec<Token>> {
    let s = regex.as_bytes();
    // Must be `^` ... `.*`; sqlwild_to_regex always brackets the body this way.
    if s.len() < 3 || s[0] != b'^' || s[s.len() - 2] != b'.' || s[s.len() - 1] != b'*' {
        return None;
    }
    if !regex.is_ascii() {
        return None;
    }
    let body = &s[1..s.len() - 2];
    let mut tokens = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'\\' => {
                // Escaped literal: `\X` -> X.
                let c = *body.get(i + 1)?;
                tokens.push(Token::Lit(c));
                i += 2;
            }
            b'[' => {
                let (class, next) = parse_class(body, i)?;
                tokens.push(class);
                i = next;
            }
            b'.' => {
                tokens.push(Token::Any);
                i += 1;
            }
            // Bare regex metacharacters should never appear unescaped in a
            // sqlwild_to_regex body; if one does, defer to the real engine.
            b'$' | b'^' | b'*' | b'+' | b'?' | b'(' | b')' | b'{' | b'}' | b'|' | b']' => {
                return None;
            }
            c => {
                tokens.push(Token::Lit(c));
                i += 1;
            }
        }
    }
    Some(tokens)
}

/// Parse a positive `[...]` class starting at `body[start] == '['`, returning the
/// `Class` token and the index just past `]`. `None` (fall back) for negation,
/// escapes, nesting, an unterminated class, or an inverted range — cases the
/// real engine must adjudicate to stay byte-identical.
fn parse_class(body: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut j = start + 1;
    if body.get(j) == Some(&b'^') {
        return None; // negation never occurs (sqlwild escapes '^'); defer if seen.
    }
    let mut ranges: Vec<(u8, u8)> = Vec::new();
    while j < body.len() && body[j] != b']' {
        let c = body[j];
        if c == b'\\' || c == b'[' {
            return None; // escapes / nesting: let the real engine decide.
        }
        // `c-d` is a range only when '-' is followed by a non-']' char; a '-' at
        // the end of the class is a literal '-'.
        if body.get(j + 1) == Some(&b'-') && body.get(j + 2).is_some_and(|&n| n != b']') {
            let hi = body[j + 2];
            if c > hi {
                return None; // inverted range => regex error; defer for parity.
            }
            ranges.push((c, hi));
            j += 3;
        } else {
            ranges.push((c, c));
            j += 1;
        }
    }
    if j >= body.len() {
        return None; // unterminated class.
    }
    Some((Token::Class(ranges), j + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_db(definitions: &[(&str, Option<&str>, i32)]) -> Db {
        use crate::tables::{serialize_artifact, Element, Pattern, VinSchema, VpicData};

        let mut strings = vec![String::new()];
        let mut intern = |s: &str| -> u32 {
            if let Some(i) = strings.iter().position(|v| v == s) {
                return i as u32;
            }
            strings.push(s.into());
            (strings.len() - 1) as u32
        };
        let patterns = definitions
            .iter()
            .enumerate()
            .map(|(i, &(key, regex, element))| Pattern {
                id: i as i32,
                vinschemaid: 1,
                keys: intern(key),
                keys_regex: intern(regex.unwrap_or("")),
                elementid: element,
                attributeid: 0,
                createdon_key: 0,
                specificity: 0,
                has_bracket: regex.is_some(),
            })
            .collect();
        let mut arena_bytes = Vec::new();
        let mut arena_offsets = vec![0];
        for s in strings {
            arena_bytes.extend_from_slice(s.as_bytes());
            arena_offsets.push(arena_bytes.len() as u32);
        }
        let data = VpicData {
            arena_bytes,
            arena_offsets,
            pattern: patterns,
            vinschema: vec![VinSchema {
                id: 1,
                tobeqced: false,
            }],
            element: [1, 2, 3, 26, 114]
                .into_iter()
                .map(|id| Element {
                    id,
                    name: 0,
                    code: 0,
                    isprivate: id == 2,
                    groupname: 0,
                    datatype: 0,
                    decode: 0,
                    decode_present: id != 3,
                    weight: 0,
                })
                .collect(),
            wmi: vec![],
            wmi_vinschema: vec![],
            make_model: vec![],
            wmi_make: vec![],
            enginemodel: vec![],
            enginemodelpattern: vec![],
            defaultvalue: vec![],
            vinexception: vec![],
            conversion: vec![],
            lookups: vec![],
            cover: vec![],
            vspecschema: vec![],
            vspecschemapattern: vec![],
            vspecpattern: vec![],
            vspecschemamodel: vec![],
            vspecschemayear: vec![],
        };
        Db::from_bytes(&serialize_artifact(&data, 1)).unwrap()
    }

    #[test]
    fn indexed_matches_equal_a_row_scan_including_duplicates_and_fallbacks() {
        let db = indexed_db(&[
            ("", None, 1),
            ("A*", None, 1),
            ("A*", None, 114),
            ("A*", None, 2),
            ("A*", None, 3),
            ("A*", None, 26),
            ("A*", None, 999),
            ("A_", None, 1),
            ("A[BC]", Some("^A[BC].*"), 1),
            ("[AB]*", Some("^[AB]..*"), 1),
            ("*[AB]X", Some("^.[AB]X.*"), 1),
            ("***", None, 1),
            ("ABC", None, 1),
            ("A|B", Some("^(A|B).*$"), 1),
            ("[", Some("["), 1),
            ("______________Z", None, 1),
            // Identical string ids in opposite matching modes must stay separate.
            ("^A.*", None, 1),
            ("A", Some("^A.*"), 1),
            ("A#", None, 2),
            ("##", None, 999),
        ]);
        let index = db.pattern_index(1).unwrap();
        assert_eq!(index.formula_rows, vec![18, 19]);
        let alphabet = b"ABCX_*!\n";
        let mut inputs = vec![String::new(), "______________Z".into(), "^A.*".into()];
        for &a in alphabet {
            inputs.push(String::from_utf8(vec![a]).unwrap());
            for &b in alphabet {
                inputs.push(String::from_utf8(vec![a, b]).unwrap());
                for &c in alphabet {
                    inputs.push(String::from_utf8(vec![a, b, c]).unwrap());
                }
            }
        }
        for input in inputs {
            let expected: Vec<u32> = db
                .patterns_for(1)
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    let eligible = db
                        .pattern_element_ok()
                        .get(p.elementid.to_native() as usize)
                        .copied()
                        .unwrap_or(false);
                    let matched = if p.has_bracket {
                        Regex::new(db.s(p.keys_regex.to_native()))
                            .ok()
                            .is_some_and(|re| re.is_match(&input))
                    } else {
                        like_match(input.as_bytes(), db.s(p.keys.to_native()).as_bytes())
                    };
                    (eligible && matched).then_some(i as u32)
                })
                .collect();
            let mut actual = index.hits(&db, &input);
            actual.sort_unstable();
            assert_eq!(actual, expected, "input {input:?}");
        }
    }

    #[test]
    fn indexes_belong_to_the_database_and_are_shared_between_threads() {
        let a = indexed_db(&[("[AB]", Some("^[AB].*"), 1)]);
        let b = indexed_db(&[("[XY]", Some("^[XY].*"), 1)]);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    assert_eq!(a.pattern_index(1).unwrap().hits(&a, "A"), vec![0]);
                    assert!(b.pattern_index(1).unwrap().hits(&b, "A").is_empty());
                    assert_eq!(b.pattern_index(1).unwrap().hits(&b, "X"), vec![0]);
                });
            }
        });
        assert!(a.pattern_index(999).is_none());
    }

    #[test]
    fn plain_like_prefix_and_wildcards() {
        assert!(like_match(b"CM826|3A004352", b"CM82*"));
        assert!(like_match(b"CM826|3A004352", b"*****|*A"));
        assert!(!like_match(b"CM826|3A004352", b"CN82*"));
        assert!(!like_match(b"CM8", b"CM826"));
    }

    #[test]
    fn bracket_regex_matches() {
        let re = sqlwild_to_regex("CM82[67]");
        assert_eq!(re, "^CM82[67].*");
        assert!(Matcher::compile(&re).is_match("CM826|3A004352"));
        assert!(!Matcher::compile(&re).is_match("CM825|3A004352"));
    }

    /// Every real bracket key of a spread of real WMIs must decide exactly what
    /// the regex engine decides. The byte-set form is a rewrite of the matching
    /// engine, so the archive's own keys — not just hand-written shapes — are the
    /// bar.
    #[test]
    fn byte_sets_agree_with_regex_over_archive_keys() {
        let db = crate::db::Db::embedded_raw();
        if !db.is_loaded() {
            eprintln!("skipping: artifact not built");
            return;
        }
        let inputs = [
            "CM826|3A004352",
            "WX7C5|BA123456",
            "A7561|PC008269",
            "TFW1E|DFC10312",
            "",
            "0",
        ];
        let mut checked = 0usize;
        for wmi in ["1HG", "5UX", "JH4", "1FT", "WBA", "3VW", "KMH"] {
            for wmiid in db.wmi_ids_for_str(wmi) {
                for wvs in db.wmi_vinschema_for(wmiid) {
                    for p in db.patterns_for(wvs.vinschemaid.to_native()) {
                        if !p.has_bracket {
                            continue;
                        }
                        let rs = db.s(p.keys_regex.to_native());
                        let m = Matcher::compile(rs);
                        let re = Regex::new(rs).ok();
                        for inp in inputs {
                            assert_eq!(
                                m.is_match(inp),
                                re.as_ref().is_some_and(|r| r.is_match(inp)),
                                "mismatch for archive regex {rs} on {inp:?}"
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "expected real bracket keys, saw {checked}");
    }

    /// The token fast path must agree with the real engine on every shape the
    /// grammar produces — literals, escaped `|`, `.`, ranges, multi-ranges,
    /// trailing/leading `-`, and out-of-range inputs.
    #[test]
    fn token_path_agrees_with_regex() {
        let keys = [
            "CM82[67]",
            "*****|*[0-9]",
            "**[A-D]",
            "[0-9A-Z]",
            "[04-9]",
            "AB*[12]C",
            "[A-Z0-9]*|*",
            "1234*",
            "[01347BDE]",
            "*|*[0-9A-Z]",
        ];
        let inputs = [
            "CM826|3A004352",
            "CM825|3A004352",
            "12345|6789ABCD",
            "ABCDE|FGHIJKLM",
            "0",
            "",
            "A1B2C3|D4E5F6G7",
            "ZZZZZ|ZZZZZZZZ",
        ];
        for k in keys {
            let rs = sqlwild_to_regex(k);
            let re = Regex::new(&rs).unwrap();
            let m = Matcher::compile(&rs);
            // Force the fast path is actually exercised (not silently falling back).
            assert!(matches!(m, Matcher::Sets(_)), "expected fast path for {rs}");
            for inp in inputs {
                assert_eq!(
                    m.is_match(inp),
                    re.is_match(inp),
                    "mismatch for regex {rs} on input {inp:?}"
                );
            }
        }
    }
}
