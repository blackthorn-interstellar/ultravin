//! Key matching: the plain SQL-`LIKE` branch and the bracket-class regex branch.
//!
//! `var_keys` (e.g. `CM826|3A004352`) is matched against a pattern's `keys`.
//! Plain keys use `LIKE replace(keys,'*','_') || '%'`; bracket keys use the
//! Postgres `~` regex produced by [`sqlwild_to_regex`] (a port of the SQL of
//! the same name, since the stored `keys_regex` column is absent from the dump).

use regex::Regex;

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
    for (i, &k) in keys.iter().enumerate() {
        if k == b'*' || k == b'_' {
            continue;
        }
        if var_keys[i] != k {
            return false;
        }
    }
    true
}

/// SQL `var_keys ~ keys_regex` for the bracket branch.
pub fn regex_match(regex: &str, var_keys: &str) -> bool {
    Regex::new(regex)
        .map(|r| r.is_match(var_keys))
        .unwrap_or(false)
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
        assert!(regex_match(&re, "CM826|3A004352"));
        assert!(!regex_match(&re, "CM825|3A004352"));
    }
}
