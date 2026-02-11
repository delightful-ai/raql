use crate::{
    ast::AstTerm,
    plan::{PlannedProgramData, PlannedTerm},
    typed::TypedTerm,
};

/// Marker trait for valid `Program` phases.
pub trait ProgramPhase:
    Copy + Clone + Default + core::fmt::Debug + Eq + PartialEq + 'static
{
    /// Term node type used in this phase.
    type Term;
    /// Extra phase-specific data attached to `Program`.
    type PhaseData: Default + Clone + core::fmt::Debug;
}

/// AST phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AstPhase;

/// Typed phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypedPhase;

/// Resolved phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResolvedPhase;

/// Planned phase marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlannedPhase;

impl ProgramPhase for AstPhase {
    type Term = AstTerm;
    type PhaseData = ();
}

impl ProgramPhase for TypedPhase {
    type Term = TypedTerm;
    type PhaseData = ();
}

impl ProgramPhase for ResolvedPhase {
    type Term = AstTerm;
    type PhaseData = ();
}

impl ProgramPhase for PlannedPhase {
    type Term = PlannedTerm;
    type PhaseData = PlannedProgramData;
}
