use core::{fmt, marker::PhantomData};

use la_arena::{Idx, RawIdx};

/// Strongly typed arena index.
///
/// The wrapped index is intentionally not constructible from raw integers
/// outside this crate, which keeps semantic layers from forging IDs.
pub struct ArenaId<T> {
    raw: RawIdx,
    marker: PhantomData<fn() -> T>,
}

impl<T> Copy for ArenaId<T> {}

impl<T> Clone for ArenaId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for ArenaId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for ArenaId<T> {}

impl<T> PartialOrd for ArenaId<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for ArenaId<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.raw.into_u32().cmp(&other.raw.into_u32())
    }
}

impl<T> core::hash::Hash for ArenaId<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.raw.into_u32().hash(state);
    }
}

impl<T> ArenaId<T> {
    /// Creates an ID from a typed arena index.
    pub(crate) const fn from_idx(idx: Idx<T>) -> Self {
        Self {
            raw: idx.into_raw(),
            marker: PhantomData,
        }
    }

    /// Creates an ID from a raw index.
    #[cfg(test)]
    pub(crate) const fn from_raw(raw: RawIdx) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Returns the wrapped typed index.
    pub(crate) const fn as_idx(self) -> Idx<T> {
        Idx::from_raw(self.raw)
    }

    /// Returns the wrapped raw index.
    pub(crate) const fn into_raw(self) -> RawIdx {
        self.raw
    }

    /// Returns the numeric index.
    pub const fn as_u32(self) -> u32 {
        self.into_raw().into_u32()
    }

    /// Returns the numeric index as `usize`.
    pub const fn as_usize(self) -> usize {
        self.as_u32() as usize
    }
}

impl<T> fmt::Debug for ArenaId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArenaId({})", self.as_u32())
    }
}

impl<T> fmt::Display for ArenaId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_u32().fmt(f)
    }
}

/// Host-stable identifier used at IR boundaries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StableId(u64);

impl StableId {
    /// Creates a new stable ID from a raw `u64`.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the underlying `u64`.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for StableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StableId({:#018x})", self.0)
    }
}

impl fmt::Display for StableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}
