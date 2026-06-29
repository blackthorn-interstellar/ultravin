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

use std::cell::RefCell;

use regex::Regex;

use crate::hash::IntMap;

thread_local! {
    /// Per-thread cache of compiled bracket matchers, keyed by the interned
    /// `keys_regex` string id. The archive is immutable, so a given id always maps
    /// to the same pattern; caching turns the hot path from "compile per pattern
    /// per decode" into one compile per distinct pattern per worker thread.
    static MATCHER_CACHE: RefCell<IntMap<u32, Matcher>> = RefCell::new(IntMap::default());
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
    #[inline]
    fn matches(&self, b: u8) -> bool {
        match self {
            Token::Any => b != b'\n',
            Token::Lit(c) => b == *c,
            Token::Class(ranges) => ranges.iter().any(|&(lo, hi)| lo <= b && b <= hi),
        }
    }
}

/// A compiled bracket matcher: the fast fixed-length-prefix token path, or the
/// real `regex` engine for anything the parser doesn't fully recognise (`None`
/// preserves the old "compile error => never match" behaviour).
enum Matcher {
    Tokens(Vec<Token>),
    Fallback(Option<Regex>),
}

impl Matcher {
    fn compile(regex: &str) -> Matcher {
        match parse_tokens(regex) {
            Some(tokens) => Matcher::Tokens(tokens),
            None => Matcher::Fallback(Regex::new(regex).ok()),
        }
    }

    #[inline]
    fn is_match(&self, haystack: &str) -> bool {
        match self {
            Matcher::Tokens(tokens) => {
                let b = haystack.as_bytes();
                if b.len() < tokens.len() {
                    return false;
                }
                tokens.iter().zip(b).all(|(t, &c)| t.matches(c))
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

/// SQL `var_keys ~ keys_regex` for the bracket branch. `regex_id` is the
/// interned `keys_regex` string id, `regex` its text; the compiled matcher is
/// memoized per thread. Behaviour is identical to compiling `regex` fresh every
/// call (same `is_match`, same `false` on a compile error).
pub fn regex_match_cached(regex_id: u32, regex: &str, var_keys: &str) -> bool {
    MATCHER_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let entry = cache
            .entry(regex_id)
            .or_insert_with(|| Matcher::compile(regex));
        entry.is_match(var_keys)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(regex_match_cached(1, &re, "CM826|3A004352"));
        assert!(!regex_match_cached(2, &re, "CM825|3A004352"));
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
            assert!(
                matches!(m, Matcher::Tokens(_)),
                "expected fast path for {rs}"
            );
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
