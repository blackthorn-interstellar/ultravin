//! XXX resolution: a port of `fElementAttributeValue`. Every `value == "XXX"`
//! item is rewritten to its looked-up name (or the raw attribute id when the
//! element has no lookup table or the id is absent).

use crate::db::Db;
use crate::decode::DecodingItem;
use crate::tables::element_lookup_tag;

/// Resolve one (element, attribute) pair to a display value, mirroring
/// `fElementAttributeValue`: a known lookup element does `select name into v`, so
/// it yields the looked-up name on a hit and NULL (-> empty) on a miss — never
/// the raw id. Only the proc's ELSE branch (an element with no lookup table)
/// returns the raw attribute id.
pub fn felement_attribute_value(db: &Db, element_id: i32, attribute_id: &str) -> String {
    match element_lookup_tag(element_id) {
        Some(tag) => attribute_id
            .parse::<i32>()
            .ok()
            .and_then(|id| db.lookup(tag, id))
            .map(str::to_string)
            .unwrap_or_default(),
        None => attribute_id.to_string(),
    }
}

/// Rewrite every `XXX` item value in place.
pub fn resolve_xxx(db: &Db, items: &mut [DecodingItem]) {
    for it in items.iter_mut() {
        if it.value == "XXX" {
            it.value = std::borrow::Cow::Owned(felement_attribute_value(
                db,
                it.element_id,
                &it.attribute_id,
            ));
        }
    }
}
