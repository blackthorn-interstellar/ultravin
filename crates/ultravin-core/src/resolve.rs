//! XXX resolution: a port of `fElementAttributeValue`. Every `value == "XXX"`
//! item is rewritten to its looked-up name (or the raw attribute id when the
//! element has no lookup table or the id is absent).

use crate::db::Db;
use crate::decode::DecodingItem;
use crate::tables::element_lookup_tag;

/// Resolve one (element, attribute) pair to a display value.
pub fn felement_attribute_value(db: &Db, element_id: i32, attribute_id: &str) -> String {
    if let Some(tag) = element_lookup_tag(element_id) {
        if let Ok(id) = attribute_id.parse::<i32>() {
            if let Some(name) = db.lookup(tag, id) {
                return name.to_string();
            }
        }
    }
    attribute_id.to_string()
}

/// Rewrite every `XXX` item value in place.
pub fn resolve_xxx(db: &Db, items: &mut [DecodingItem]) {
    for it in items.iter_mut() {
        if it.value == "XXX" {
            it.value = felement_attribute_value(db, it.element_id, &it.attribute_id);
        }
    }
}
