use std::collections::BTreeMap;

use crate::{DefId, DefKind};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct CoreLookupIndex {
    defs: BTreeMap<DefId, CoreDefMetadata>,
}

impl CoreLookupIndex {
    pub(crate) fn record_def(&mut self, def_id: DefId, name: &str, kind: DefKind, path: &str) {
        let metadata = self
            .defs
            .entry(def_id)
            .or_insert_with(|| CoreDefMetadata::new(name, kind, path));
        metadata.name = name.to_owned().into_boxed_str();
        metadata.kind = kind;
        metadata.path = path.to_owned().into_boxed_str();
    }

    pub(crate) fn mark_public(&mut self, def_id: DefId, is_public: bool) {
        if let Some(metadata) = self.defs.get_mut(&def_id) {
            metadata.is_public = Some(is_public);
        }
    }

    pub(crate) fn mark_in_test(&mut self, def_id: DefId, in_test: bool) {
        if let Some(metadata) = self.defs.get_mut(&def_id) {
            metadata.in_test = Some(in_test);
        }
    }

    pub(crate) fn def_name(&self, def_id: DefId) -> Option<&str> {
        self.defs.get(&def_id).map(|metadata| metadata.name.as_ref())
    }

    pub(crate) fn def_kind(&self, def_id: DefId) -> Option<DefKind> {
        self.defs.get(&def_id).map(|metadata| metadata.kind)
    }

    pub(crate) fn def_path(&self, def_id: DefId) -> Option<&str> {
        self.defs.get(&def_id).map(|metadata| metadata.path.as_ref())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct CoreDefMetadata {
    name: Box<str>,
    kind: DefKind,
    path: Box<str>,
    is_public: Option<bool>,
    in_test: Option<bool>,
}

impl CoreDefMetadata {
    fn new(name: &str, kind: DefKind, path: &str) -> Self {
        Self {
            name: name.to_owned().into_boxed_str(),
            kind,
            path: path.to_owned().into_boxed_str(),
            is_public: None,
            in_test: None,
        }
    }
}
