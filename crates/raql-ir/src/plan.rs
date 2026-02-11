use la_arena::Arena;
use thiserror::Error;

use crate::{
    ids::{ArenaId, StableId},
    ty::TyId,
    value::{ScalarValue, Symbol},
};

/// ID of a planned term node.
pub type PlannedTermId = ArenaId<PlannedTerm>;

/// ID of a plan operation node.
pub type PlanOpId = ArenaId<PlanOp>;

/// Reference to a host-provided scalar input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScalarInputRef {
    /// Stable data source identifier.
    pub source: StableId,
    /// Input key at the source.
    pub key: Symbol,
}

impl ScalarInputRef {
    /// Creates a new scalar input reference.
    pub fn new(source: StableId, key: impl Into<Symbol>) -> Self {
        Self {
            source,
            key: key.into(),
        }
    }
}

/// Planned term with fully known type and execution references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTerm {
    /// Resolved type.
    pub ty: TyId,
    /// Term payload.
    pub kind: PlannedTermKind,
}

impl PlannedTerm {
    /// Builds a scalar planned term.
    pub fn scalar(ty: TyId, value: ScalarValue) -> Self {
        Self {
            ty,
            kind: PlannedTermKind::Scalar(value),
        }
    }

    /// Builds a host input planned term.
    pub fn input(ty: TyId, input: ScalarInputRef) -> Self {
        Self {
            ty,
            kind: PlannedTermKind::Input(input),
        }
    }

    /// Builds a computed planned term.
    pub fn eval(ty: TyId, op: PlanOpId, args: impl IntoIterator<Item = PlannedTermId>) -> Self {
        Self {
            ty,
            kind: PlannedTermKind::Eval {
                op,
                args: args.into_iter().collect::<Vec<_>>().into_boxed_slice(),
            },
        }
    }
}

/// Planned term variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedTermKind {
    /// Scalar literal.
    Scalar(ScalarValue),
    /// Host input scalar.
    Input(ScalarInputRef),
    /// Computed value produced by a plan op.
    Eval {
        /// Producer operation.
        op: PlanOpId,
        /// Operand terms.
        args: Box<[PlannedTermId]>,
    },
}

/// Planned relational/logical operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOp {
    /// Scan a host source by stable ID.
    Scan {
        /// Stable host source identifier.
        source: StableId,
    },
    /// Filter operation.
    Filter {
        /// Upstream operation.
        input: PlanOpId,
        /// Predicate term.
        predicate: PlannedTermId,
    },
    /// Projection operation.
    Project {
        /// Upstream operation.
        input: PlanOpId,
        /// Expression terms to project.
        expressions: Box<[PlannedTermId]>,
    },
    /// Join operation.
    Join {
        /// Left input operation.
        left: PlanOpId,
        /// Right input operation.
        right: PlanOpId,
        /// Join predicate term.
        on: PlannedTermId,
    },
}

/// Errors raised when plan structure invariants are violated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanInvariantError {
    /// Root operation is not present in the operation arena.
    #[error("root op id {root} is out of bounds for {len} ops")]
    InvalidRootOp {
        /// Invalid root op.
        root: PlanOpId,
        /// Arena length at validation time.
        len: usize,
    },
    /// Requested operation ID is not present in the operation arena.
    #[error("op id {id} is out of bounds for {len} ops")]
    InvalidOpId {
        /// Invalid operation ID.
        id: PlanOpId,
        /// Arena length at validation time.
        len: usize,
    },
}

/// Extra phase data attached to planned programs.
#[derive(Debug, Clone, Default)]
pub struct PlannedProgramData {
    ops: Arena<PlanOp>,
    root_op: Option<PlanOpId>,
}

impl PlannedProgramData {
    /// Creates an empty plan arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a plan operation.
    pub fn alloc_op(&mut self, op: PlanOp) -> PlanOpId {
        ArenaId::from_idx(self.ops.alloc(op))
    }

    /// Returns an immutable operation reference.
    pub fn op(&self, id: PlanOpId) -> Result<&PlanOp, PlanInvariantError> {
        self.try_op(id).ok_or(PlanInvariantError::InvalidOpId {
            id,
            len: self.ops.len(),
        })
    }

    /// Returns a mutable operation reference.
    pub fn op_mut(&mut self, id: PlanOpId) -> Result<&mut PlanOp, PlanInvariantError> {
        if !self.contains_op(id) {
            return Err(PlanInvariantError::InvalidOpId {
                id,
                len: self.ops.len(),
            });
        }
        Ok(&mut self.ops[id.as_idx()])
    }

    /// Attempts to get an operation by ID.
    pub fn try_op(&self, id: PlanOpId) -> Option<&PlanOp> {
        self.contains_op(id).then(|| &self.ops[id.as_idx()])
    }

    /// Attempts to get a mutable operation by ID.
    pub fn try_op_mut(&mut self, id: PlanOpId) -> Option<&mut PlanOp> {
        self.contains_op(id).then(|| &mut self.ops[id.as_idx()])
    }

    /// Returns an iterator over operations.
    pub fn iter_ops(
        &self,
    ) -> impl ExactSizeIterator<Item = (PlanOpId, &PlanOp)> + DoubleEndedIterator + Clone {
        self.ops.iter().map(|(id, op)| (ArenaId::from_idx(id), op))
    }

    /// Returns true when this arena contains no operations.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Returns the operation count.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns the root operation if one has been set.
    pub fn root_op(&self) -> Option<PlanOpId> {
        self.root_op
    }

    /// Sets the root operation.
    pub fn set_root_op(&mut self, root_op: PlanOpId) -> Result<(), PlanInvariantError> {
        if !self.contains_op(root_op) {
            return Err(PlanInvariantError::InvalidRootOp {
                root: root_op,
                len: self.ops.len(),
            });
        }
        self.root_op = Some(root_op);
        Ok(())
    }

    /// Returns true if the ID belongs to this operation arena.
    pub fn contains_op(&self, id: PlanOpId) -> bool {
        id.as_usize() < self.ops.len()
    }
}

#[cfg(test)]
mod tests {
    use la_arena::RawIdx;

    use crate::plan::{PlanOp, PlanOpId, PlannedProgramData};

    #[test]
    fn root_op_assignment_requires_known_id() {
        let mut data = PlannedProgramData::new();
        let root = data.alloc_op(PlanOp::Scan {
            source: crate::StableId::new(7),
        });

        data.set_root_op(root).expect("valid root op id");
        assert_eq!(data.root_op(), Some(root));

        let invalid = PlanOpId::from_raw(RawIdx::from_u32(3));
        let err = data
            .set_root_op(invalid)
            .expect_err("expected root-op invariant failure");
        assert_eq!(
            err.to_string(),
            "root op id 3 is out of bounds for 1 ops".to_string()
        );
    }
}
