use std::collections::{BTreeMap, BTreeSet};

use hir::Function;

use crate::{DefId, DefKind};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct CoreLookupIndex {
    defs: BTreeMap<DefId, CoreDefMetadata>,
    functions: BTreeMap<DefId, Function>,
}

impl CoreLookupIndex {
    pub(crate) fn record_def(
        &mut self,
        def_id: DefId,
        name: &str,
        kind: DefKind,
        path: &str,
        source_rel_path: Option<&str>,
    ) {
        let metadata = self
            .defs
            .entry(def_id)
            .or_insert_with(|| CoreDefMetadata::new(name, kind, path, source_rel_path));
        metadata.name = name.to_owned().into_boxed_str();
        metadata.kind = kind;
        metadata.path = path.to_owned().into_boxed_str();
        metadata.source_rel_path = source_rel_path.map(|path| path.to_owned().into_boxed_str());
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

    pub(crate) fn record_function(&mut self, def_id: DefId, function: Function) {
        self.functions.insert(def_id, function);
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

    pub(crate) fn contains_def(&self, def_id: DefId) -> bool {
        self.defs.contains_key(&def_id)
    }

    pub(crate) fn def_ids(&self) -> impl Iterator<Item = DefId> + '_ {
        self.defs.keys().copied()
    }

    pub(crate) fn function(&self, def_id: DefId) -> Option<Function> {
        self.functions.get(&def_id).copied()
    }

    pub(crate) fn invalidate_paths(&mut self, changed_rel_paths: &BTreeSet<String>) {
        if changed_rel_paths.is_empty() {
            return;
        }
        let invalid_defs = self
            .defs
            .iter()
            .filter_map(|(def_id, metadata)| {
                metadata
                    .source_rel_path
                    .as_deref()
                    .filter(|path| changed_rel_paths.contains(*path))
                    .map(|_| *def_id)
            })
            .collect::<Vec<_>>();
        for def_id in invalid_defs {
            self.defs.remove(&def_id);
            self.functions.remove(&def_id);
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct CoreDefMetadata {
    name: Box<str>,
    kind: DefKind,
    path: Box<str>,
    source_rel_path: Option<Box<str>>,
    is_public: Option<bool>,
    in_test: Option<bool>,
}

impl CoreDefMetadata {
    fn new(name: &str, kind: DefKind, path: &str, source_rel_path: Option<&str>) -> Self {
        Self {
            name: name.to_owned().into_boxed_str(),
            kind,
            path: path.to_owned().into_boxed_str(),
            source_rel_path: source_rel_path.map(|path| path.to_owned().into_boxed_str()),
            is_public: None,
            in_test: None,
        }
    }
}
