use core::fmt;

use la_arena::{Arena, Idx, RawIdx};
use rustc_hash::FxHashMap;

use crate::value::Symbol;

/// Canonical type ID returned by [`TypeInterner`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyId(Idx<Ty>);

impl TyId {
    /// Returns the raw arena index.
    pub(crate) const fn into_raw(self) -> RawIdx {
        self.0.into_raw()
    }

    /// Returns the numeric index.
    pub const fn as_u32(self) -> u32 {
        self.into_raw().into_u32()
    }

    pub(crate) const fn as_idx(self) -> Idx<Ty> {
        self.0
    }
}

impl fmt::Debug for TyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TyId({})", self.as_u32())
    }
}

impl fmt::Display for TyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_u32().fmt(f)
    }
}

/// Core type representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// Placeholder for unresolved type information.
    Unknown,
    /// Unit type.
    Unit,
    /// Boolean type.
    Bool,
    /// Signed 64-bit integer type.
    I64,
    /// UTF-8 string type.
    String,
    /// Homogeneous list.
    List(TyId),
    /// Positional tuple.
    Tuple(Box<[TyId]>),
    /// Named fields.
    Record(Box<[(Symbol, TyId)]>),
}

/// Canonical type store with deduplicating interning.
#[derive(Debug, Clone, Default)]
pub struct TypeInterner {
    arena: Arena<Ty>,
    index: FxHashMap<Ty, TyId>,
}

impl TypeInterner {
    /// Creates a new empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns a type and returns its stable `TyId`.
    pub fn intern(&mut self, ty: Ty) -> TyId {
        if let Some(existing) = self.index.get(&ty) {
            return *existing;
        }

        let id = TyId(self.arena.alloc(ty.clone()));
        self.index.insert(ty, id);
        id
    }

    /// Looks up an interned type by ID.
    pub fn get(&self, id: TyId) -> &Ty {
        &self.arena[id.as_idx()]
    }

    /// Returns an iterator over interned types.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (TyId, &Ty)> + DoubleEndedIterator + Clone {
        self.arena.iter().map(|(id, ty)| (TyId(id), ty))
    }

    /// Returns true if an ID belongs to this interner.
    pub fn contains_id(&self, id: TyId) -> bool {
        (u32::from(id.into_raw()) as usize) < self.arena.len()
    }

    /// Returns the count of unique interned types.
    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Returns true if no types are interned.
    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    /// Attempts to get a type by ID.
    pub fn try_get(&self, id: TyId) -> Option<&Ty> {
        if self.contains_id(id) {
            Some(&self.arena[id.as_idx()])
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ty::{Ty, TypeInterner},
        value::Symbol,
    };

    #[test]
    fn interner_deduplicates_identical_types() {
        let mut interner = TypeInterner::new();
        let a = interner.intern(Ty::Bool);
        let b = interner.intern(Ty::Bool);

        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn interner_handles_nested_types() {
        let mut interner = TypeInterner::new();
        let string = interner.intern(Ty::String);
        let rec = interner.intern(Ty::Record(
            vec![(Symbol::from("name"), string)].into_boxed_slice(),
        ));

        assert_ne!(string, rec);
        assert_eq!(
            interner.get(rec),
            &Ty::Record(vec![(Symbol::from("name"), string)].into_boxed_slice())
        );
    }
}
