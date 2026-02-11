use la_arena::Arena;
use thiserror::Error;

use crate::{
    diag::Diagnostics,
    ids::ArenaId,
    phase::{AstPhase, PlannedPhase, ProgramPhase, ResolvedPhase, TypedPhase},
    ty::TypeInterner,
};

/// Term ID type for a given phase.
pub type TermId<P> = ArenaId<<P as ProgramPhase>::Term>;

/// Errors raised when constructing or mutating program structure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProgramInvariantError {
    /// Root term is not present in the term arena.
    #[error("root term id {root} is out of bounds for {len} terms")]
    InvalidRootTerm {
        /// Invalid root term ID.
        root: u32,
        /// Arena length at validation time.
        len: usize,
    },
    /// Requested term ID is not present in the term arena.
    #[error("term id {id} is out of bounds for {len} terms")]
    InvalidTermId {
        /// Invalid term ID.
        id: u32,
        /// Arena length at validation time.
        len: usize,
    },
}

/// Phase-typed IR program.
#[derive(Debug, Clone)]
pub struct Program<P: ProgramPhase> {
    terms: Arena<P::Term>,
    root: TermId<P>,
    type_interner: TypeInterner,
    diagnostics: Diagnostics,
    phase_data: P::PhaseData,
}

/// AST program type.
pub type AstProgram = Program<AstPhase>;
/// Type-checked program type.
pub type TypedProgram = Program<TypedPhase>;
/// Resolved program type.
pub type ResolvedProgram = Program<ResolvedPhase>;
/// Planned program type.
pub type PlannedProgram = Program<PlannedPhase>;

impl<P: ProgramPhase> Program<P> {
    /// Creates a new program with a root term.
    pub fn new(root_term: P::Term) -> Self {
        Self::with_phase_data(root_term, P::PhaseData::default())
    }

    /// Creates a new program with explicit phase data.
    pub fn with_phase_data(root_term: P::Term, phase_data: P::PhaseData) -> Self {
        let mut terms = Arena::new();
        let root = ArenaId::from_idx(terms.alloc(root_term));
        Self {
            terms,
            root,
            type_interner: TypeInterner::default(),
            diagnostics: Diagnostics::default(),
            phase_data,
        }
    }

    /// Reconstructs a program from arenas and root IDs.
    pub fn from_parts(
        terms: Arena<P::Term>,
        root: TermId<P>,
        type_interner: TypeInterner,
        diagnostics: Diagnostics,
        phase_data: P::PhaseData,
    ) -> Result<Self, ProgramInvariantError> {
        if !Self::contains_id(&terms, root) {
            return Err(ProgramInvariantError::InvalidRootTerm {
                root: root.as_u32(),
                len: terms.len(),
            });
        }
        Ok(Self {
            terms,
            root,
            type_interner,
            diagnostics,
            phase_data,
        })
    }

    /// Allocates and returns a new term ID.
    pub fn alloc_term(&mut self, term: P::Term) -> TermId<P> {
        ArenaId::from_idx(self.terms.alloc(term))
    }

    /// Returns the root term ID.
    pub fn root(&self) -> TermId<P> {
        self.root
    }

    /// Reassigns the root term ID.
    pub fn set_root(&mut self, root: TermId<P>) -> Result<(), ProgramInvariantError> {
        if !self.contains_term(root) {
            return Err(ProgramInvariantError::InvalidRootTerm {
                root: root.as_u32(),
                len: self.terms.len(),
            });
        }
        self.root = root;
        Ok(())
    }

    /// Returns true if this program contains the given term ID.
    pub fn contains_term(&self, id: TermId<P>) -> bool {
        Self::contains_id(&self.terms, id)
    }

    /// Returns an immutable reference to a term.
    pub fn term(&self, id: TermId<P>) -> Result<&P::Term, ProgramInvariantError> {
        self.try_term(id)
            .ok_or(ProgramInvariantError::InvalidTermId {
                id: id.as_u32(),
                len: self.terms.len(),
            })
    }

    /// Returns a mutable reference to a term.
    pub fn term_mut(&mut self, id: TermId<P>) -> Result<&mut P::Term, ProgramInvariantError> {
        if !self.contains_term(id) {
            return Err(ProgramInvariantError::InvalidTermId {
                id: id.as_u32(),
                len: self.terms.len(),
            });
        }
        Ok(&mut self.terms[id.as_idx()])
    }

    /// Attempts to get a term by ID.
    pub fn try_term(&self, id: TermId<P>) -> Option<&P::Term> {
        self.contains_term(id).then(|| &self.terms[id.as_idx()])
    }

    /// Attempts to get a mutable term by ID.
    pub fn try_term_mut(&mut self, id: TermId<P>) -> Option<&mut P::Term> {
        self.contains_term(id).then(|| &mut self.terms[id.as_idx()])
    }

    /// Returns an iterator over terms.
    pub fn iter_terms(
        &self,
    ) -> impl ExactSizeIterator<Item = (TermId<P>, &P::Term)> + DoubleEndedIterator + Clone {
        self.terms
            .iter()
            .map(|(id, term)| (ArenaId::from_idx(id), term))
    }

    /// Returns the number of terms.
    pub fn term_len(&self) -> usize {
        self.terms.len()
    }

    /// Returns the shared type interner.
    pub fn type_interner(&self) -> &TypeInterner {
        &self.type_interner
    }

    /// Returns a mutable shared type interner.
    pub fn type_interner_mut(&mut self) -> &mut TypeInterner {
        &mut self.type_interner
    }

    /// Returns diagnostics collected for this program.
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// Returns mutable diagnostics storage.
    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diagnostics
    }

    /// Returns phase-specific program data.
    pub fn phase_data(&self) -> &P::PhaseData {
        &self.phase_data
    }

    /// Returns mutable phase-specific program data.
    pub fn phase_data_mut(&mut self) -> &mut P::PhaseData {
        &mut self.phase_data
    }

    fn contains_id(terms: &Arena<P::Term>, id: TermId<P>) -> bool {
        id.as_usize() < terms.len()
    }
}

#[cfg(test)]
mod tests {
    use la_arena::{Arena, RawIdx};

    use crate::{
        ast::{AstTerm, AstTermKind},
        phase::AstPhase,
        program::{Program, ProgramInvariantError, TermId},
        value::Symbol,
    };

    #[test]
    fn root_reassignment_requires_valid_id() {
        let mut program = Program::<AstPhase>::new(AstTerm::var("root"));
        let valid = program.alloc_term(AstTerm::var("next"));
        program.set_root(valid).expect("valid root term id");

        match &program
            .term(program.root())
            .expect("root should be valid")
            .kind
        {
            AstTermKind::Var(sym) => assert_eq!(sym, &Symbol::from("next")),
            other => panic!("unexpected node: {other:?}"),
        }
    }

    #[test]
    fn from_parts_rejects_out_of_bounds_root() {
        let mut arena = Arena::new();
        arena.alloc(AstTerm::var("only"));

        let invalid = TermId::<AstPhase>::from_raw(RawIdx::from_u32(7));
        let err = Program::<AstPhase>::from_parts(
            arena,
            invalid,
            Default::default(),
            Default::default(),
            (),
        )
        .expect_err("expected invalid root");
        assert_eq!(
            err,
            ProgramInvariantError::InvalidRootTerm { root: 7, len: 1 }
        );
    }

    #[test]
    fn set_root_rejects_unknown_term_id() {
        let mut program = Program::<AstPhase>::new(AstTerm::var("root"));
        let invalid = TermId::<AstPhase>::from_raw(RawIdx::from_u32(5));

        let err = program
            .set_root(invalid)
            .expect_err("expected invalid root assignment");
        assert_eq!(
            err,
            ProgramInvariantError::InvalidRootTerm { root: 5, len: 1 }
        );
    }
}
