//! Model-year resolution: a port of `fVinModelYear2` plus the wrapper's single
//! best-year choice (the `altMY` ±30 swap). The 4-pass best-of orchestration is
//! W2; W1 decodes against this one year.

use crate::db::Db;
use crate::tables::NULL_I32;
use crate::wmi::vin_wmi;

/// Raw `fVinModelYear2`: `None` when position 10 is unmapped, a negative value
/// when the year is inconclusive, otherwise the positive model year. `carLT`
/// (passenger car / MPV / light truck) triggers the position-7 −30 adjustment.
pub fn vin_model_year_raw(vin: &str, db: &Db, current_year: i32) -> Option<i32> {
    let b = vin.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let p = b[9];
    let mut my: i32 = match p {
        b'A'..=b'H' => 2010 + (p - b'A') as i32,
        b'J'..=b'N' => 2010 + (p - b'A') as i32 - 1,
        b'P' => 2023,
        b'R'..=b'T' => 2010 + (p - b'A') as i32 - 3,
        b'V'..=b'Y' => 2010 + (p - b'A') as i32 - 4,
        b'1'..=b'9' => 2031 + (p - b'1') as i32,
        _ => return None,
    };

    let mut conclusive = false;
    let wmi = vin_wmi(vin);
    if let Some(w) = db.wmi_any(&wmi) {
        let vt = w.vehicletypeid.to_native();
        let car_lt = matches!(vt, 2 | 7) || (vt == 3 && w.trucktypeid.to_native() == 1);
        let pos7 = b.get(6).copied().unwrap_or(b' ');
        if car_lt && pos7.is_ascii_digit() {
            my -= 30;
            conclusive = true;
        }
        if car_lt && pos7.is_ascii_uppercase() {
            conclusive = true;
        }
        if my > current_year + 2 {
            my -= 30;
            conclusive = true;
        }
    }

    Some(if conclusive { my } else { -my })
}

/// The model-year candidates the wrapper feeds into the decode passes.
pub struct YearPlan {
    /// Primary candidate (pass 3). `None` when position 10 is unmapped.
    pub rmy: Option<i32>,
    /// Alternate candidate (pass 4), set only when the year is inconclusive.
    pub omy: Option<i32>,
    /// `false` when `fVinModelYear2` was inconclusive (drives pass 4 + note 156).
    pub conclusive: bool,
}

/// Port of the wrapper's year computation: `rmy`/`omy`/`conclusive`, including
/// the `altMY` ±30 schema-count swap (only when conclusive). The dead descriptor
/// pass is skipped (it never runs in the proc — see PLAN.md).
pub fn resolve_years(vin: &str, db: &Db, current_year: i32) -> YearPlan {
    let v_limit = current_year + 2;
    match vin_model_year_raw(vin, db, current_year) {
        None => YearPlan {
            rmy: None,
            omy: None,
            conclusive: true,
        },
        Some(raw) => {
            let conclusive = raw > 0;
            let mut rmy = raw.abs();
            let omy = if conclusive { None } else { Some(rmy - 30) };
            if conclusive {
                let alt = if (1980..=v_limit - 30).contains(&rmy) {
                    Some(rmy + 30)
                } else if (1980 + 30..=v_limit).contains(&rmy) {
                    Some(rmy - 30)
                } else {
                    None
                };
                if let Some(a) = alt {
                    if a != rmy && schema_count(vin, db, rmy) == 0 && schema_count(vin, db, a) > 0 {
                        rmy = a;
                    }
                }
            }
            YearPlan {
                rmy: Some(rmy),
                omy,
                conclusive,
            }
        }
    }
}

/// Count of WMI schemas covering `year` for this VIN's WMI.
fn schema_count(vin: &str, db: &Db, year: i32) -> i32 {
    let wmi = vin_wmi(vin);
    let Some(w) = db.wmi_any(&wmi) else {
        return 0;
    };
    db.wmi_vinschema_for(w.id.to_native())
        .iter()
        .filter(|r| {
            let to = if r.yearto.to_native() == NULL_I32 {
                2999
            } else {
                r.yearto.to_native()
            };
            year >= r.yearfrom.to_native() && year <= to
        })
        .count() as i32
}
