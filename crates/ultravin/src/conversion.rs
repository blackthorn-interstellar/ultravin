//! Port of the `vpic.conversion` dynamic-formula evaluation in `spvindecode_core`.
//!
//! For each conversion the proc builds `select (<formula with #x# replaced>)::varchar(500)`
//! and runs it; on any error the result is `'0'`. The committed formula set is
//! finite and uniform — `#x# <op> <const>` with `op` in `{*, /}` — so this is a
//! small expression evaluator that reproduces PostgreSQL `numeric` arithmetic
//! (scale + rounding) exactly:
//!
//! * multiplication: result scale = sum of the two operand display scales;
//! * division: result scale = PostgreSQL `select_div_scale` (>= 16 significant
//!   digits), rounded half away from zero.
//!
//! All arithmetic is done on decimal digit strings, so there is no overflow for
//! any input length and the output is byte-identical to `(expr)::varchar`.

/// Evaluate `formula` (e.g. `"#x# / 16.387064 "`) with `#x#` = `value`.
/// Returns `"0"` on any parse/evaluation error, mirroring the proc's exception.
pub fn eval(formula: &str, value: &str) -> String {
    eval_inner(formula, value).unwrap_or_else(|| "0".to_string())
}

fn eval_inner(formula: &str, value: &str) -> Option<String> {
    let pos = formula.find("#x#")?;
    let rest = formula[pos + 3..].trim_start();
    let op = rest.chars().next()?;
    let const_str = rest[op.len_utf8()..].trim();
    let x = Dec::parse(value)?;
    let c = Dec::parse(const_str)?;
    match op {
        '*' => Some(mul(&x, &c)),
        '/' => {
            if c.is_zero() {
                return None; // division by zero -> exception -> '0'
            }
            Some(div(&x, &c))
        }
        _ => None,
    }
}

/// A parsed decimal literal: sign, magnitude digit string (no dot/sign, may have
/// leading zeros), and display scale (digits after the decimal point).
struct Dec {
    neg: bool,
    mag: String,
    dscale: usize,
}

impl Dec {
    /// Parse a PostgreSQL plain numeric/integer literal. Exponents are rejected
    /// (they would be `float8` in PostgreSQL and never appear in the data).
    fn parse(s: &str) -> Option<Dec> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut chars = s.chars().peekable();
        let mut neg = false;
        match chars.peek() {
            Some('+') => {
                chars.next();
            }
            Some('-') => {
                neg = true;
                chars.next();
            }
            _ => {}
        }
        let mut int_part = String::new();
        let mut frac_part = String::new();
        let mut seen_dot = false;
        let mut any_digit = false;
        for ch in chars {
            if ch == '.' {
                if seen_dot {
                    return None;
                }
                seen_dot = true;
            } else if ch.is_ascii_digit() {
                any_digit = true;
                if seen_dot {
                    frac_part.push(ch);
                } else {
                    int_part.push(ch);
                }
            } else {
                return None;
            }
        }
        if !any_digit {
            return None;
        }
        Some(Dec {
            dscale: frac_part.len(),
            mag: format!("{int_part}{frac_part}"),
            neg,
        })
    }

    fn is_zero(&self) -> bool {
        !self.mag.bytes().any(|b| b != b'0')
    }

    /// Base-10000 normalized `(weight, first digit group)` used by `select_div_scale`.
    fn weight_and_firstdigit(&self) -> (i32, i64) {
        let stripped = self.mag.trim_start_matches('0');
        if stripped.is_empty() {
            return (0, 0);
        }
        let a_len = stripped.len() as i32;
        let msd_pow = a_len - 1 - self.dscale as i32;
        let w = msd_pow.div_euclid(4);
        let n_take = (msd_pow - 4 * w + 1) as usize; // 1..=4
        let mut lead: String = stripped.chars().take(n_take).collect();
        while lead.len() < n_take {
            lead.push('0');
        }
        (w, lead.parse::<i64>().unwrap_or(0))
    }
}

/// PostgreSQL `select_div_scale` (numeric.c): at least `NUMERIC_MIN_SIG_DIGITS`
/// (16) significant digits, but never below either operand's display scale.
fn select_div_scale(num: &Dec, den: &Dec) -> usize {
    let (w1, f1) = num.weight_and_firstdigit();
    let (w2, f2) = den.weight_and_firstdigit();
    let mut qweight = w1 - w2;
    if f1 <= f2 {
        qweight -= 1;
    }
    let mut rscale = 16 - qweight * 4;
    rscale = rscale.max(num.dscale as i32).max(den.dscale as i32).max(0);
    rscale.min(1000) as usize
}

fn mul(x: &Dec, c: &Dec) -> String {
    let scale = x.dscale + c.dscale;
    let mag = big_mul(&x.mag, &c.mag);
    apply_sign(format_fixed(&mag, scale), x.neg ^ c.neg)
}

fn div(x: &Dec, c: &Dec) -> String {
    let scale = select_div_scale(x, c);
    let den_int: u128 = c.mag.trim_start_matches('0').parse().unwrap_or(0);
    let exp = scale as i32 - x.dscale as i32 + c.dscale as i32;
    let mag = if exp >= 0 {
        let mut dividend = x.mag.clone();
        for _ in 0..exp {
            dividend.push('0');
        }
        let (q, r) = long_div(&dividend, den_int);
        round_up_if(q, r.saturating_mul(2) >= den_int)
    } else {
        let denom = den_int * 10u128.pow((-exp) as u32);
        let (q, r) = long_div(&x.mag, denom);
        round_up_if(q, r.saturating_mul(2) >= denom)
    };
    apply_sign(format_fixed(&mag, scale), x.neg)
}

/// Long division of a decimal digit string by `divisor`, returning the quotient
/// digit string and the final remainder (`< divisor`).
fn long_div(dividend: &str, divisor: u128) -> (String, u128) {
    let mut rem: u128 = 0;
    let mut out = String::with_capacity(dividend.len());
    for b in dividend.bytes() {
        let cur = rem * 10 + (b - b'0') as u128;
        out.push((b'0' + (cur / divisor) as u8) as char);
        rem = cur % divisor;
    }
    (out, rem)
}

/// Increment a nonnegative decimal digit string by one when `inc` is set.
fn round_up_if(s: String, inc: bool) -> String {
    if !inc {
        return s;
    }
    let mut bytes = s.into_bytes();
    let mut i = bytes.len();
    loop {
        if i == 0 {
            bytes.insert(0, b'1');
            break;
        }
        i -= 1;
        if bytes[i] == b'9' {
            bytes[i] = b'0';
        } else {
            bytes[i] += 1;
            break;
        }
    }
    String::from_utf8(bytes).expect("ascii digits")
}

/// Format a magnitude digit string as fixed point with exactly `scale` fractional
/// digits (PostgreSQL `numeric` text output preserves trailing zeros).
fn format_fixed(mag: &str, scale: usize) -> String {
    let mut s: String = mag.trim_start_matches('0').to_string();
    if s.is_empty() {
        s.push('0');
    }
    if scale == 0 {
        return s;
    }
    while s.len() <= scale {
        s.insert(0, '0');
    }
    let cut = s.len() - scale;
    format!("{}.{}", &s[..cut], &s[cut..])
}

/// Prepend `-` only for a negative, nonzero result.
fn apply_sign(body: String, neg: bool) -> String {
    if neg && body.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        format!("-{body}")
    } else {
        body
    }
}

/// Multiply two decimal digit strings (schoolbook, no overflow).
fn big_mul(a: &str, b: &str) -> String {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    if a.is_empty() || b.is_empty() {
        return "0".to_string();
    }
    let av: Vec<u32> = a.bytes().rev().map(|c| (c - b'0') as u32).collect();
    let bv: Vec<u32> = b.bytes().rev().map(|c| (c - b'0') as u32).collect();
    let mut res = vec![0u32; av.len() + bv.len()];
    for (i, &x) in av.iter().enumerate() {
        for (j, &y) in bv.iter().enumerate() {
            res[i + j] += x * y;
        }
    }
    let mut carry = 0u32;
    for d in res.iter_mut() {
        let cur = *d + carry;
        *d = cur % 10;
        carry = cur / 10;
    }
    debug_assert_eq!(carry, 0);
    let mut out: String = res
        .iter()
        .rev()
        .map(|d| (b'0' + *d as u8) as char)
        .collect();
    let trimmed = out.trim_start_matches('0');
    out = if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Oracle ground truth: `select (<formula with #x# = value>)::varchar(500)`.
    #[test]
    fn division_matches_postgres() {
        assert_eq!(eval("#x# / 16.387064 ", "223"), "13.6082949331252993");
        assert_eq!(eval("#x# / 1000.", "223"), "0.22300000000000000000");
        assert_eq!(eval("#x# / 16.387064 ", "5733"), "349.8491248951001839");
        assert_eq!(eval("#x# / 0.016387064 ", "5"), "305.1187204736614198");
        assert_eq!(eval("#x# / 16.387064 ", "0"), "0.00000000000000000000");
        assert_eq!(eval("#x# / 0.016387064 ", "1"), "61.0237440947322840");
        assert_eq!(eval("#x# / 16.387064 ", "100.00"), "6.1023744094732284");
        assert_eq!(eval("#x# / 1000.", "1.0"), "0.00100000000000000000");
        assert_eq!(eval("#x# / 16.387064 ", "9999"), "610.1764172032281072");
        assert_eq!(eval("#x# / 16.387064 ", "12345678"), "753379.494947966274");
    }

    #[test]
    fn multiplication_matches_postgres() {
        assert_eq!(eval("#x# * 16.387064 ", "5733"), "93947.037912");
        assert_eq!(eval("#x# * 1000", "5.7"), "5700.0");
        assert_eq!(eval("#x# * 1000", "1"), "1000");
        assert_eq!(eval("#x# * 1000", "1.0"), "1000.0");
        assert_eq!(eval("#x# * 1000", "1.40"), "1400.00");
        assert_eq!(eval("#x# * 16.387064 ", "5.7"), "93.4062648");
        assert_eq!(eval("#x# * 16.387064 ", "1.44206163"), "23.63115622275432");
        assert_eq!(eval("#x# * 0.016387064 ", "105.3"), "1.7255578392");
        assert_eq!(eval("#x# * 1000", "0.6"), "600.0");
        assert_eq!(eval("#x# * 1000", "1.85173823"), "1851.73823000");
        assert_eq!(eval("#x# * 16.387064 ", "12345678"), "202309415.509392");
    }

    #[test]
    fn errors_yield_zero() {
        assert_eq!(eval("#x# / 16.387064 ", "abc"), "0");
        assert_eq!(eval("#x# / 16.387064 ", ""), "0");
        assert_eq!(eval("#x# / 16.387064 ", "1.2.3"), "0");
        assert_eq!(eval("#x# * 16.387064 ", "5 7"), "0");
    }

    #[test]
    fn whitespace_in_value_is_tolerated() {
        // PostgreSQL parses ` 223 / 16.387064 ` fine.
        assert_eq!(eval("#x# / 16.387064 ", " 223 "), "13.6082949331252993");
    }
}
