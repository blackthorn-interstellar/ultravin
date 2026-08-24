//! `WMIYearValidChars` staleness scan (opt-in, `--stale-cache-report`).
//!
//! `vpic.WMIYearValidChars` is a materialized snapshot of
//! `vpic.fExtractValidCharsPerWmiYear`, and `spvindecode_errorcode` prefers the
//! snapshot — it calls the function only when a (wmi, year) cell has no rows at
//! all. NHTSA does not refresh the snapshot when a monthly rebuild drops pattern
//! rows, so the oracle's correction charset can contradict the pattern rows the
//! same dump ships (see `docs/KNOWN_DEVIATIONS.md`). ultravin always recomputes,
//! so every stale cell is a potential divergence.
//!
//! This diffs each cell of the dump's cache against the recompute from that same
//! dump's pattern rows, at the granularity the proc consumes: the cache row is
//! `(wmi, year, position, char)`, so a cell's contents are a set of
//! `(VIN position, allowed char)` pairs and the diff is reported per position.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use ultravin::{recompute_valid_chars, Db};

use crate::artifact::copy_columns;

/// The dump table this scan reads.
pub const TABLE: &str = "wmiyearvalidchars";

/// The opt-out list `spvindecode_errorcode` consults beside the cache.
pub const EXCEPTIONS_TABLE: &str = "wmiyearvalidchars_cacheexceptions";

/// Characters allowed at one VIN position, sorted.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct PosChars {
    position: i32,
    chars: String,
}

/// One (wmi, year) cell whose cache disagrees with the recompute.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct StaleCell {
    wmi: String,
    year: i32,
    only_in_cache: Vec<PosChars>,
    only_in_recompute: Vec<PosChars>,
    /// The dump's own pattern rows leave this cell no schema coverage at all, so
    /// the cache is the only thing keeping the oracle from falling through to an
    /// empty charset.
    recompute_empty: bool,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Summary {
    stale_cells: usize,
    rows_only_in_cache: u64,
    rows_only_in_recompute: u64,
    cells_recompute_empty: usize,
    /// Distinct WMIs in `wmiyearvalidchars_cacheexceptions`, whose cells this
    /// scan drops. Always 0 so far; anything else means the assumption the whole
    /// scan rests on has moved and a human has to re-read the proc (see
    /// [`CacheExceptions`]).
    cache_exception_wmis: usize,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Report {
    dump: String,
    cells_total: usize,
    stale_cells: Vec<StaleCell>,
    summary: Summary,
}

/// WMIs listed in `wmiyearvalidchars_cacheexceptions`, collected from that
/// table's COPY block.
///
/// The proc's cache read carries `AND var_wmi NOT IN (SELECT DISTINCT wmi FROM
/// vpic.WMIYearValidChars_CacheExceptions)`, so a listed WMI is supposed to skip
/// the cache and take the `fExtractValidCharsPerWmiYear` fallback — i.e. agree
/// with ultravin — and its cells must not be listed as divergence surface.
///
/// The table has shipped empty every month so far, which is fortunate, because
/// the subquery does not do what it reads like: this table's only WMI column is
/// the quoted, upper-case `"WMI"`, so the unquoted `wmi` in the subquery matches
/// nothing in its own `FROM` and resolves outward to `WMIYearValidChars.wmi`
/// instead. A single row in this table would therefore make the subquery return
/// the outer WMI itself and switch *every* WMI to the fallback. Either reading
/// invalidates this scan's premise, which is why a non-zero count fails the
/// refresh gate rather than quietly changing what gets excluded.
#[derive(Default)]
pub struct CacheExceptions {
    wmi: usize,
    wmis: BTreeSet<String>,
}

impl CacheExceptions {
    /// Bind to the exceptions table's `COPY ... FROM stdin;` line, or `None` if
    /// it does not carry a WMI column.
    pub fn from_copy_line(line: &str) -> Option<CacheExceptions> {
        let cols = copy_columns(line);
        let wmi = cols
            .iter()
            .position(|c| c.trim_matches('"').eq_ignore_ascii_case("wmi"))?;
        Some(CacheExceptions {
            wmi,
            wmis: BTreeSet::new(),
        })
    }

    /// Feed one data row line. The column is declared `character varying(6)[]`,
    /// so the COPY text is a Postgres array literal (`{ABC,DEF}`); a plain
    /// scalar is accepted too, in case NHTSA ever fixes the type.
    pub fn feed(&mut self, line: &str) {
        let Some(field) = line.split('\t').nth(self.wmi) else {
            return;
        };
        if field == "\\N" {
            return;
        }
        for wmi in field.trim_matches(['{', '}']).split(',') {
            let wmi = wmi.trim().trim_matches('"');
            if !wmi.is_empty() {
                self.wmis.insert(wmi.to_string());
            }
        }
    }

    /// The collected WMIs.
    pub fn into_wmis(self) -> BTreeSet<String> {
        self.wmis
    }
}

/// Cache rows collected from the dump's `wmiyearvalidchars` COPY block.
///
/// A cell's `(position, char)` pairs are packed one per `u16` rather than into a
/// `BTreeSet` per position per cell. On the 2026_08 dump that is 8.8M pairs in
/// 241,380 `Vec`s — ~18 MB of payload, the rest `Vec` growth slack and the
/// `HashMap`'s `String` keys. Measured peak RSS with the scan on is ~57 MB above
/// the same import with it off (1047 MB vs 987 MB).
pub struct CacheRows {
    cells: HashMap<(String, i32), Vec<u16>>,
    wmi: usize,
    year: usize,
    position: usize,
    ch: usize,
}

impl CacheRows {
    /// Bind to a `COPY vpic.wmiyearvalidchars (...) FROM stdin;` line, or `None`
    /// if it does not carry the four columns the scan needs.
    pub fn from_copy_line(line: &str) -> Option<CacheRows> {
        let cols = copy_columns(line);
        let at = |name: &str| cols.iter().position(|c| c.trim_matches('"') == name);
        Some(CacheRows {
            cells: HashMap::new(),
            wmi: at("wmi")?,
            year: at("year")?,
            position: at("position")?,
            ch: at("char")?,
        })
    }

    /// Feed one data row line (tab-separated COPY body).
    pub fn feed(&mut self, line: &str) {
        let f: Vec<&str> = line.split('\t').collect();
        let (Some(wmi), Some(Ok(year))) = (f.get(self.wmi), f.get(self.year).map(|y| y.parse()))
        else {
            return;
        };
        // A row with a NULL position or char still makes the cell non-empty for
        // the proc's fallback test, so register the cell either way.
        let pairs = self.cells.entry(((*wmi).to_string(), year)).or_default();
        let (Some(Ok(pos)), Some(c)) = (
            f.get(self.position).map(|p| p.parse::<u8>()),
            // `\N` is COPY's NULL, not a character: the column is
            // `varchar(1)`, so every real value is exactly one byte.
            f.get(self.ch).and_then(|c| match c.as_bytes() {
                [b] => Some(*b),
                _ => None,
            }),
        ) else {
            return;
        };
        pairs.push(u16::from(pos) << 8 | u16::from(c));
    }

    /// Diff every cell against `db` (built from the same dump) and report.
    ///
    /// Cells whose WMI the proc is told to skip the cache for are not divergence
    /// surface, so they never reach the list.
    pub fn report(self, db: &Db, dump: &str, exceptions: &BTreeSet<String>) -> Report {
        let cells_total = self.cells.len();
        let mut cells: Vec<((String, i32), Vec<u16>)> = self.cells.into_iter().collect();
        cells.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut stale_cells: Vec<StaleCell> = Vec::new();
        let mut summary = Summary {
            stale_cells: 0,
            rows_only_in_cache: 0,
            rows_only_in_recompute: 0,
            cells_recompute_empty: 0,
            cache_exception_wmis: exceptions.len(),
        };
        for ((wmi, year), pairs) in cells {
            if exceptions.contains(&wmi) {
                continue;
            }
            let mut cached: BTreeMap<i32, BTreeSet<char>> = BTreeMap::new();
            for p in pairs {
                cached
                    .entry(i32::from(p >> 8))
                    .or_default()
                    .insert(char::from((p & 0xff) as u8));
            }
            let fresh = recompute_valid_chars(db, &wmi, year);
            let only_in_cache = extra_chars(&cached, &fresh);
            let only_in_recompute = extra_chars(&fresh, &cached);
            if only_in_cache.is_empty() && only_in_recompute.is_empty() {
                continue;
            }
            summary.rows_only_in_cache += count_chars(&only_in_cache);
            summary.rows_only_in_recompute += count_chars(&only_in_recompute);
            let recompute_empty = fresh.is_empty();
            summary.cells_recompute_empty += usize::from(recompute_empty);
            stale_cells.push(StaleCell {
                wmi,
                year,
                only_in_cache,
                only_in_recompute,
                recompute_empty,
            });
        }
        summary.stale_cells = stale_cells.len();
        Report {
            dump: dump.to_string(),
            cells_total,
            stale_cells,
            summary,
        }
    }
}

/// Positions where `a` allows characters `b` does not, ascending.
fn extra_chars(
    a: &BTreeMap<i32, BTreeSet<char>>,
    b: &BTreeMap<i32, BTreeSet<char>>,
) -> Vec<PosChars> {
    let none = BTreeSet::new();
    a.iter()
        .filter_map(|(position, chars)| {
            let chars: String = chars.difference(b.get(position).unwrap_or(&none)).collect();
            (!chars.is_empty()).then_some(PosChars {
                position: *position,
                chars,
            })
        })
        .collect()
}

/// Cache rows a positional diff accounts for (one row per allowed character).
fn count_chars(diff: &[PosChars]) -> u64 {
    diff.iter().map(|p| p.chars.chars().count() as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactBuilder;

    /// A four-row artifact: WMI `AAA` covered by one schema for 2020-2021 whose
    /// two pattern keys allow `A` at VIN position 4 and `B`/`C` at position 5.
    fn tiny_db() -> Db {
        let blocks: &[(&str, &[&str])] = &[
            (
                "COPY vpic.wmi (id, wmi, manufacturerid, makeid, vehicletypeid, trucktypeid, \
                 publicavailabilitydate, createdon, updatedon) FROM stdin;",
                &["1\tAAA\t1\t1\t2\t\\N\t2000-01-01 00:00:00\t2000-01-01 00:00:00\t\\N"],
            ),
            (
                "COPY vpic.wmi_vinschema (id, wmiid, vinschemaid, yearfrom, yearto) FROM stdin;",
                &["1\t1\t1\t2020\t2021"],
            ),
            (
                "COPY vpic.vinschema (id, name, sourcewmi, tobeqced) FROM stdin;",
                &["1\ts\tAAA\tf"],
            ),
            (
                "COPY vpic.pattern (id, vinschemaid, keys, elementid, attributeid, createdon, \
                 updatedon) FROM stdin;",
                &[
                    "1\t1\tAB\t1\tx\t2000-01-01 00:00:00\t\\N",
                    "2\t1\tAC\t1\ty\t2000-01-01 00:00:00\t\\N",
                ],
            ),
        ];
        let mut builder = ArtifactBuilder::default();
        for (header, rows) in blocks {
            let table = header
                .strip_prefix("COPY vpic.")
                .and_then(|r| r.split([' ', '(']).next())
                .unwrap();
            builder.begin_copy(table, header);
            for row in *rows {
                builder.feed(row);
            }
            builder.end_copy();
        }
        let (bytes, _) = builder.build(0, 2021);
        Db::from_bytes(&bytes).unwrap()
    }

    fn pos(position: i32, chars: &str) -> PosChars {
        PosChars {
            position,
            chars: chars.to_string(),
        }
    }

    const CACHE_HEADER: &str =
        "COPY vpic.wmiyearvalidchars (id, wmi, year, \"position\", \"char\") FROM stdin;";

    fn tiny_cache() -> CacheRows {
        let mut rows = CacheRows::from_copy_line(CACHE_HEADER).unwrap();
        for row in [
            // Cache keeps a dead 'Z' at position 4 and has not picked up 'C' at 5.
            "1\tAAA\t2020\t4\tA",
            "2\tAAA\t2020\t4\tZ",
            "3\tAAA\t2020\t5\tB",
            // Agrees with the recompute — must not appear in the report.
            "4\tAAA\t2021\t4\tA",
            "5\tAAA\t2021\t5\tB",
            "6\tAAA\t2021\t5\tC",
            // No schema covers BBB at all: the cache is all that is left.
            "7\tBBB\t2020\t4\tQ",
        ] {
            rows.feed(row);
        }
        rows
    }

    #[test]
    fn scan_reports_both_diff_directions_and_empty_recomputes() {
        let report = tiny_cache().report(&tiny_db(), "2020_01", &BTreeSet::new());
        assert_eq!(report.dump, "2020_01");
        assert_eq!(report.cells_total, 3);
        assert_eq!(
            report.summary,
            Summary {
                stale_cells: 2,
                rows_only_in_cache: 2,
                rows_only_in_recompute: 1,
                cells_recompute_empty: 1,
                cache_exception_wmis: 0,
            }
        );
        assert_eq!(report.stale_cells[0].wmi, "AAA");
        assert_eq!(report.stale_cells[0].year, 2020);
        assert_eq!(report.stale_cells[0].only_in_cache, vec![pos(4, "Z")]);
        assert_eq!(report.stale_cells[0].only_in_recompute, vec![pos(5, "C")]);
        assert!(!report.stale_cells[0].recompute_empty);
        assert_eq!(report.stale_cells[1].wmi, "BBB");
        assert_eq!(report.stale_cells[1].only_in_cache, vec![pos(4, "Q")]);
        assert!(report.stale_cells[1].only_in_recompute.is_empty());
        assert!(report.stale_cells[1].recompute_empty);
    }

    #[test]
    fn a_null_char_row_registers_the_cell_without_inventing_a_character() {
        let mut rows = CacheRows::from_copy_line(CACHE_HEADER).unwrap();
        // `\N` is COPY's NULL. Taking its first byte would put a literal `\` in
        // the cache's charset and report every such cell as stale.
        rows.feed("1\tAAA\t2020\t4\t\\N");
        rows.feed("2\tAAA\t2020\t\\N\tA");
        let report = rows.report(&tiny_db(), "2020_01", &BTreeSet::new());
        assert_eq!(report.cells_total, 1);
        assert_eq!(report.stale_cells[0].only_in_cache, vec![]);
        assert_eq!(
            report.stale_cells[0].only_in_recompute,
            vec![pos(4, "A"), pos(5, "BC")]
        );
    }

    #[test]
    fn an_excepted_wmi_is_not_divergence_surface() {
        let exceptions = BTreeSet::from(["AAA".to_string()]);
        let report = tiny_cache().report(&tiny_db(), "2020_01", &exceptions);
        assert_eq!(report.summary.cache_exception_wmis, 1);
        assert_eq!(report.summary.stale_cells, 1);
        assert_eq!(report.stale_cells[0].wmi, "BBB");
    }

    #[test]
    fn exceptions_parse_the_array_typed_wmi_column() {
        let header =
            "COPY vpic.wmiyearvalidchars_cacheexceptions (\"WMI\", \"CreatedOn\", \"Id\") \
                      FROM stdin;";
        let mut ex = CacheExceptions::from_copy_line(header).unwrap();
        ex.feed("{AAA,BBB}\t{2020-01-01 00:00:00}\t1");
        ex.feed("CCC\t\\N\t2"); // a scalar, if the column type is ever fixed
        ex.feed("\\N\t\\N\t3");
        assert_eq!(
            ex.into_wmis(),
            BTreeSet::from(["AAA".to_string(), "BBB".to_string(), "CCC".to_string()])
        );
    }
}
