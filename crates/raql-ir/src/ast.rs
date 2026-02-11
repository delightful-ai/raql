use crate::{
    ids::ArenaId,
    value::{ScalarValue, Symbol},
};

/// ID of an AST term node allocated in an arena.
pub type AstTermId = ArenaId<AstTerm>;

/// Untyped IR term in the AST phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstTerm {
    /// Node payload.
    pub kind: AstTermKind,
}

impl AstTerm {
    /// Builds a scalar term.
    pub fn scalar(value: ScalarValue) -> Self {
        Self {
            kind: AstTermKind::Scalar(value),
        }
    }

    /// Builds a variable term.
    pub fn var(name: impl Into<Symbol>) -> Self {
        Self {
            kind: AstTermKind::Var(name.into()),
        }
    }

    /// Builds a call term.
    pub fn call(callee: AstTermId, args: impl IntoIterator<Item = AstTermId>) -> Self {
        Self {
            kind: AstTermKind::Call {
                callee,
                args: args.into_iter().collect::<Vec<_>>().into_boxed_slice(),
            },
        }
    }

    /// Builds a `let` term.
    pub fn let_in(binding: impl Into<Symbol>, value: AstTermId, body: AstTermId) -> Self {
        Self {
            kind: AstTermKind::Let {
                binding: binding.into(),
                value,
                body,
            },
        }
    }
}

/// AST term variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstTermKind {
    /// Scalar literal.
    Scalar(ScalarValue),
    /// Variable reference.
    Var(Symbol),
    /// Function or predicate call.
    Call {
        /// Callee term.
        callee: AstTermId,
        /// Argument terms.
        args: Box<[AstTermId]>,
    },
    /// Local binding expression.
    Let {
        /// Binding name.
        binding: Symbol,
        /// Bound value.
        value: AstTermId,
        /// Body expression.
        body: AstTermId,
    },
}
