//! The planner's logical input IR (SPEC §10.1).
//!
//! The lang layer lowers its stratified, typechecked rules into this shape;
//! the planner sees only what it needs: predicates, goals, argument
//! boundness, and names for messages. No RA types, no execution, no value
//! representation — a constant is just "always bound".
//!
//! Selector bindings arrive as *input relations* (SPEC §10.1: "selectors
//! arrive as pre-bound input relations, e.g. `target_def`"): tiny
//! pre-materialized relations that bind all their arguments at C0.
//!
//! Aggregation/choose/witness goals are engine-level constructs; they are
//! not represented in the v0 planner slice.

use crate::mode::Binding;

/// A rule-scoped variable: an index into the owning rule's `vars` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u32);

/// One goal argument. The planner cares about boundness only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Var(Var),
    /// A literal constant — always bound; the engine holds the value.
    Const,
}

/// Index into [`Program::derived`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DerivedId(pub usize);

/// Index into [`Program::inputs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputId(pub usize);

/// Index into [`Program::builtins`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuiltinId(pub usize);

/// What a goal calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalRef {
    /// An extern predicate, named as in the catalog.
    Extern(String),
    Derived(DerivedId),
    Input(InputId),
    Builtin(BuiltinId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goal {
    pub target: GoalRef,
    pub args: Vec<Term>,
    pub negated: bool,
}

impl Goal {
    pub fn positive(target: GoalRef, args: Vec<Term>) -> Goal {
        Goal { target, args, negated: false }
    }

    pub fn negated(target: GoalRef, args: Vec<Term>) -> Goal {
        Goal { target, args, negated: true }
    }
}

/// One rule: `head :- body`. The query is a rule too (its head lists the
/// output columns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Display names of this rule's variables, indexed by [`Var`]. Used in
    /// plan errors and explain output only.
    pub vars: Vec<String>,
    pub head: Vec<Term>,
    pub body: Vec<Goal>,
}

impl Rule {
    pub fn var_name(&self, var: Var) -> &str {
        &self.vars[var.0 as usize]
    }
}

/// A derived predicate and its rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedDef {
    pub name: String,
    pub arity: usize,
    /// `.mode` assertions (SPEC §9.1): every declared pattern must be
    /// inferable (else a plan error), and once declared, the declared set
    /// is the public contract — callers may not use merely-inferable
    /// patterns beyond it.
    pub declared_modes: Option<Vec<Vec<Binding>>>,
    pub rules: Vec<Rule>,
}

/// A pre-bound input relation (selector seed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDef {
    pub name: String,
    pub arity: usize,
}

/// An engine-managed builtin with a fixed binding requirement: `Bound`
/// positions must be bound at the call; `Free` positions are computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinDef {
    pub name: String,
    pub pattern: Vec<Binding>,
}

/// A planner input program: derived predicates, input relations, builtins,
/// and the query body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub derived: Vec<DerivedDef>,
    pub inputs: Vec<InputDef>,
    pub builtins: Vec<BuiltinDef>,
    pub query: Rule,
}

impl Program {
    /// The user-facing name of a goal's target predicate.
    pub fn target_name<'p>(&'p self, target: &'p GoalRef) -> &'p str {
        match target {
            GoalRef::Extern(name) => name,
            GoalRef::Derived(id) => &self.derived[id.0].name,
            GoalRef::Input(id) => &self.inputs[id.0].name,
            GoalRef::Builtin(id) => &self.builtins[id.0].name,
        }
    }

    /// Render a goal as `name(Arg, ...)` for messages, using the owning
    /// rule's variable names.
    pub fn render_goal(&self, rule: &Rule, goal: &Goal) -> String {
        let args = goal
            .args
            .iter()
            .map(|term| match term {
                Term::Var(var) => rule.var_name(*var).to_owned(),
                Term::Const => "<const>".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let not = if goal.negated { "not " } else { "" };
        format!("{not}{}({args})", self.target_name(&goal.target))
    }
}
