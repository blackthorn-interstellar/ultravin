//! The element list of a [`DecodeResult`](crate::DecodeResult).

use std::sync::Arc;

use crate::DecodedElement;

/// A result's elements: a list of its own, or one shared with other results.
///
/// A native stream's result cache gives every VIN that hits an entry the
/// entry's list instead of a copy. Either way it reads as a slice of
/// [`DecodedElement`] and compares, prints and serializes like a `Vec`;
/// like [`Cow`](std::borrow::Cow), [`Elements::to_mut`] and
/// [`Elements::into_vec`] copy a shared list first.
#[derive(Clone, Default)]
pub struct Elements<'a>(Repr<'a>);

#[derive(Clone)]
enum Repr<'a> {
    Owned(Vec<DecodedElement<'a>>),
    Shared(Arc<Vec<DecodedElement<'a>>>),
}

impl Default for Repr<'_> {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

impl<'a> Elements<'a> {
    /// The list, copied first if another result shares it.
    pub fn to_mut(&mut self) -> &mut Vec<DecodedElement<'a>> {
        if let Repr::Shared(shared) = &self.0 {
            self.0 = Repr::Owned(shared.to_vec());
        }
        match &mut self.0 {
            Repr::Owned(elements) => elements,
            Repr::Shared(_) => unreachable!("made owned above"),
        }
    }

    /// The list, copied if another result shares it.
    pub fn into_vec(self) -> Vec<DecodedElement<'a>> {
        match self.0 {
            Repr::Owned(elements) => elements,
            Repr::Shared(shared) => Arc::unwrap_or_clone(shared),
        }
    }

    /// How many elements the list has room for, shared or not.
    pub fn capacity(&self) -> usize {
        match &self.0 {
            Repr::Owned(elements) => elements.capacity(),
            Repr::Shared(shared) => shared.capacity(),
        }
    }

    pub(crate) fn is_shared(&self) -> bool {
        matches!(self.0, Repr::Shared(_))
    }

    /// Whether both share one list.
    pub(crate) fn shares(&self, other: &Self) -> bool {
        matches!((&self.0, &other.0), (Repr::Shared(a), Repr::Shared(b)) if Arc::ptr_eq(a, b))
    }

    /// Share the list without copying it, and return another handle to it.
    pub(crate) fn share(&mut self) -> Self {
        if let Repr::Owned(elements) = &mut self.0 {
            self.0 = Repr::Shared(Arc::new(std::mem::take(elements)));
        }
        self.clone()
    }

    /// The list emptied for reuse, unless another result still shares it.
    pub(crate) fn into_buffer(self) -> Option<Vec<DecodedElement<'a>>> {
        let mut elements = match self.0 {
            Repr::Owned(elements) => elements,
            Repr::Shared(shared) => Arc::into_inner(shared)?,
        };
        crate::clear_elements(&mut elements);
        Some(elements)
    }
}

impl<'a> From<Vec<DecodedElement<'a>>> for Elements<'a> {
    fn from(elements: Vec<DecodedElement<'a>>) -> Self {
        Self(Repr::Owned(elements))
    }
}

impl<'a> std::ops::Deref for Elements<'a> {
    type Target = [DecodedElement<'a>];

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Repr::Owned(elements) => elements,
            Repr::Shared(shared) => shared,
        }
    }
}

impl<'s, 'a> IntoIterator for &'s Elements<'a> {
    type Item = &'s DecodedElement<'a>;
    type IntoIter = std::slice::Iter<'s, DecodedElement<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for Elements<'a> {
    type Item = DecodedElement<'a>;
    type IntoIter = std::vec::IntoIter<DecodedElement<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl PartialEq for Elements<'_> {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl Eq for Elements<'_> {}

impl std::fmt::Debug for Elements<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self[..].fmt(f)
    }
}

impl serde::Serialize for Elements<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self[..].serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn element(value: &str) -> DecodedElement<'_> {
        DecodedElement {
            group_name: "General",
            variable: "Make",
            value: Cow::Owned(value.to_owned()),
            element_id: 26,
            attribute_id: Cow::Borrowed("474"),
            code: "Make",
            data_type: "lookup",
            decode: "Pattern",
            source: Cow::Borrowed("Pattern"),
            pattern_id: Some(1),
            vin_schema_id: None,
            keys: Cow::Borrowed("CM8"),
            created_on: None,
            wmi_id: Some(2),
            to_be_qced: false,
        }
    }

    #[test]
    fn shared_and_owned_lists_read_print_and_serialize_like_a_vec() {
        let list = vec![element("HONDA"), element("ACURA")];
        let mut owned = Elements::from(list.clone());
        let shared = owned.share();
        assert!(owned.shares(&shared) && owned.is_shared());
        for elements in [&Elements::from(list.clone()), &shared] {
            assert_eq!(&elements[..], &list[..]);
            assert_eq!(format!("{elements:?}"), format!("{list:?}"));
            assert_eq!(
                serde_json::to_string(elements).expect("json"),
                serde_json::to_string(&list).expect("json")
            );
            assert_eq!(elements.into_iter().count(), 2);
        }
        assert_eq!(Elements::from(list.clone()), shared);
        assert!(!Elements::from(list.clone()).shares(&Elements::from(list.clone())));
        assert_eq!(shared.clone().into_iter().collect::<Vec<_>>(), list);
    }

    #[test]
    fn writes_and_buffers_never_touch_a_list_another_result_shares() {
        let mut first = Elements::from(vec![element("HONDA")]);
        let mut second = first.share();
        second.to_mut()[0].value = Cow::Borrowed("ACURA");
        assert!(!second.is_shared());
        assert_eq!(first[0].value, "HONDA");
        assert_eq!(second.into_vec()[0].value, "ACURA");

        let third = first.share();
        assert!(first.clone().into_buffer().is_none());
        assert_eq!(first.into_vec(), third.to_vec());
        let buffer = third.into_buffer().expect("the last handle");
        assert!(buffer.is_empty() && buffer.capacity() >= 1);
        assert_eq!(Elements::default().into_buffer(), Some(Vec::new()));
    }
}
