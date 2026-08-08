//! The `Keys` bracket-class mini-language, in one place.
//!
//! A `Keys` spec (e.g. `[A-C]M82[67]`) is walked position by position by the VIN
//! generator and the cover builder. Every `[...]` position needs the same
//! primitive: locate the class, iterate its members, test membership, or take its
//! first member. The `a-b` range kernel below was copy-pasted verbatim into four
//! call sites; this is the single copy. Allocation-free and byte-oriented (VIN
//! keys are ASCII).
//!
//! Deliberately NOT shared with two bracket parsers that look similar but are a
//! different language:
//! - `matcher::parse_class` parses a *regex* body (the `sqlwild_to_regex` output)
//!   with a contract to fall back to the real regex engine on anything unusual
//!   (escape, nesting, negation, inverted range). Those rejections are interleaved
//!   with the scan and pin the fast-path/regex split the token test guards.
//! - `errors::valid_chars_in_regex` is a port of the SQL `fValidCharsInRegEx`:
//!   regex-engine-backed, restricted to the VALIDCHARS alphabet and case-folded.
//!
//! Unifying either would change output.

/// The inclusive `(lo, hi)` byte ranges of a class body (the bytes between `[`
/// and `]`). A bare byte `b` is the singleton `(b, b)`; `a-b` is a range only
/// when another body byte follows the `-` (a trailing `-` is a literal). A
/// reversed range (`lo > hi`) is yielded verbatim: membership against it is
/// always false and expansion skips it, so every caller drops it the same way.
pub(crate) struct ClassRanges<'a> {
    body: &'a [u8],
    i: usize,
}

impl Iterator for ClassRanges<'_> {
    type Item = (u8, u8);

    #[inline]
    fn next(&mut self) -> Option<(u8, u8)> {
        let b = self.body;
        if self.i >= b.len() {
            return None;
        }
        let i = self.i;
        if i + 2 < b.len() && b[i + 1] == b'-' {
            self.i = i + 3;
            Some((b[i], b[i + 2]))
        } else {
            self.i = i + 1;
            Some((b[i], b[i]))
        }
    }
}

/// The `(lo, hi)` ranges of a class `body` (the bytes between the brackets).
#[inline]
pub(crate) fn class_ranges(body: &[u8]) -> ClassRanges<'_> {
    ClassRanges { body, i: 0 }
}

/// Does the class `body` accept byte `c`?
#[inline]
pub(crate) fn class_contains(body: &[u8], c: u8) -> bool {
    class_ranges(body).any(|(lo, hi)| lo <= c && c <= hi)
}

/// Locate the `[...]` class opened at `bytes[open] == b'['`: its body (the bytes
/// between the brackets) and the index just past the closing `]`. `None` for an
/// unterminated `[`, which the callers treat as a literal `[` byte.
#[inline]
pub(crate) fn class_body(bytes: &[u8], open: usize) -> Option<(&[u8], usize)> {
    let rel = bytes[open..].iter().position(|&c| c == b']')?;
    Some((&bytes[open + 1..open + rel], open + rel + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_singletons_and_reversed() {
        assert_eq!(
            class_ranges(b"67").collect::<Vec<_>>(),
            [(b'6', b'6'), (b'7', b'7')]
        );
        assert_eq!(class_ranges(b"A-C").collect::<Vec<_>>(), [(b'A', b'C')]);
        // A trailing '-' is a literal, not a range opener.
        assert_eq!(
            class_ranges(b"A-").collect::<Vec<_>>(),
            [(b'A', b'A'), (b'-', b'-')]
        );
        // A reversed range is yielded verbatim; the caller drops it.
        assert_eq!(class_ranges(b"C-A").collect::<Vec<_>>(), [(b'C', b'A')]);
    }

    #[test]
    fn membership_matches_ranges() {
        assert!(class_contains(b"A-C", b'B'));
        assert!(!class_contains(b"A-C", b'D'));
        assert!(class_contains(b"67", b'7'));
        assert!(!class_contains(b"C-A", b'B')); // reversed accepts nothing
    }

    #[test]
    fn class_body_finds_extent_or_none() {
        assert_eq!(class_body(b"[67]M", 0), Some((&b"67"[..], 4)));
        assert_eq!(class_body(b"AB[C-F]", 2), Some((&b"C-F"[..], 7)));
        assert_eq!(class_body(b"[67", 0), None); // unterminated
    }
}
