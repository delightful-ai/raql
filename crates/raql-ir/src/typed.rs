use crate::{
    ids::ArenaId,
    ty::TyId,
    value::{ScalarValue, Symbol},
};

/// ID of a typed term node allocated in an arena.
pub type TypedTermId = ArenaId<TypedTerm>;

/// Type-checked IR term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedTerm {
    /// Fully resolved type for this node.
    pub ty: TyId,
    /// Node payload.
    pub kind: TypedTermKind,
}

impl TypedTerm {
    /// Builds a scalar typed term.
    pub fn scalar(ty: TyId, value: ScalarValue) -> Self {
        Self {
            ty,
            kind: TypedTermKind::Scalar(value),
        }
    }

    /// Builds a variable typed term.
    pub fn var(ty: TyId, name: impl Into<Symbol>) -> Self {
        Self {
            ty,
            kind: TypedTermKind::Var(name.into()),
        }
    }

    /// Builds a call typed term.
    pub fn call(
        ty: TyId,
        callee: TypedTermId,
        args: impl IntoIterator<Item = TypedTermId>,
    ) -> Self {
        Self {
            ty,
            kind: TypedTermKind::Call {
                callee,
                args: args.into_iter().collect::<Vec<_>>().into_boxed_slice(),
            },
        }
    }

    /// Builds a `let` typed term.
    pub fn let_in(
        ty: TyId,
        binding: impl Into<Symbol>,
        value: TypedTermId,
        body: TypedTermId,
    ) -> Self {
        Self {
            ty,
            kind: TypedTermKind::Let {
                binding: binding.into(),
                value,
                body,
            },
        }
    }

    /// Builds an explicit cast/coercion node.
    pub fn cast(ty: TyId, value: TypedTermId, target: TyId) -> Self {
        Self {
            ty,
            kind: TypedTermKind::Cast { value, target },
        }
    }
}

/// Typed term variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedTermKind {
    /// Scalar literal.
    Scalar(ScalarValue),
    /// Variable reference.
    Var(Symbol),
    /// Function call.
    Call {
        /// Callee term.
        callee: TypedTermId,
        /// Argument terms.
        args: Box<[TypedTermId]>,
    },
    /// Local binding expression.
    Let {
        /// Binding name.
        binding: Symbol,
        /// Bound value.
        value: TypedTermId,
        /// Body expression.
        body: TypedTermId,
    },
    /// Explicit cast from one type to another.
    Cast {
        /// Value being cast.
        value: TypedTermId,
        /// Target type.
        target: TyId,
    },
}
