//! Metadata editing lives on [`Document`](crate::document::Document) itself;
//! this module holds the helpers that build a [`Metadata`] from partial edits.

use crate::document::Metadata;

/// A set of metadata edits, where `None` means "leave unchanged" and
/// `Some(None)` means "clear this field".
#[derive(Debug, Clone, Default)]
pub struct MetadataEdit {
    pub title: Option<Option<String>>,
    pub author: Option<Option<String>>,
    pub subject: Option<Option<String>>,
    pub keywords: Option<Option<String>>,
}

impl MetadataEdit {
    /// True when no field would change.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.subject.is_none()
            && self.keywords.is_none()
    }

    /// Apply these edits on top of `base`.
    pub fn apply(&self, base: &Metadata) -> Metadata {
        fn field(edit: &Option<Option<String>>, current: &Option<String>) -> Option<String> {
            match edit {
                Some(new) => new.clone(),
                None => current.clone(),
            }
        }

        Metadata {
            title: field(&self.title, &base.title),
            author: field(&self.author, &base.author),
            subject: field(&self.subject, &base.subject),
            keywords: field(&self.keywords, &base.keywords),
            creator: base.creator.clone(),
            producer: base.producer.clone(),
        }
    }
}
