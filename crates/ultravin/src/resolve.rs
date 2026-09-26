//! XXX resolution: a port of `fElementAttributeValue`. Every `value == "XXX"`
//! item is rewritten to its looked-up name (or the raw attribute id when the
//! element has no lookup table or the id is absent), and every value leaves
//! with tabs and line breaks replaced, as each output shape shows it.

use std::borrow::Cow;

use crate::db::Db;
use crate::decode::DecodingItem;
use crate::tables::element_lookup_tag;

/// Resolve one (element, attribute) pair to a display value, mirroring
/// `fElementAttributeValue`: a known lookup element does `select name into v`, so
/// it yields the looked-up name on a hit and NULL (-> empty) on a miss — never
/// the raw id. Only the proc's ELSE branch (an element with no lookup table)
/// returns the raw attribute id.
pub fn felement_attribute_value<'a>(
    db: &'a Db,
    element_id: i32,
    attribute_id: Cow<'a, str>,
) -> Cow<'a, str> {
    match element_lookup_tag(element_id) {
        Some(tag) => attribute_id
            .parse::<i32>()
            .ok()
            .and_then(|id| db.lookup(tag, id))
            .map(Cow::Borrowed)
            .unwrap_or_default(),
        None => attribute_id,
    }
}

/// Rewrite every `XXX` item value in place, and scrub every value. Most
/// values are lookup names, whose tables already know which need scrubbing.
pub fn resolve_xxx<'a>(db: &'a Db, items: &mut [DecodingItem<'a>]) {
    for it in items.iter_mut() {
        it.value = if it.value == "XXX" {
            match element_lookup_tag(it.element_id) {
                Some(tag) => it
                    .attribute_id
                    .parse::<i32>()
                    .ok()
                    .and_then(|id| db.lookup_scrubbed(tag, id))
                    .unwrap_or_default(),
                None => crate::scrub_value(it.attribute_id.clone()),
            }
        } else {
            crate::scrub_value(std::mem::take(&mut it.value))
        };
    }
}
