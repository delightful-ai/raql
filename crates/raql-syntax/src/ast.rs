use crate::source::{SourceMap, Spanned};
use smol_str::SmolStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program<P> {
    sources: SourceMap,
    phase: P,
}

impl<P> Program<P> {
    #[must_use]
    pub fn new(sources: SourceMap, phase: P) -> Self {
        Self { sources, phase }
    }

    #[must_use]
    pub fn sources(&self) -> &SourceMap {
        &self.sources
    }

    #[must_use]
    pub fn phase(&self) -> &P {
        &self.phase
    }

    #[must_use]
    pub fn into_sources(self) -> SourceMap {
        self.sources
    }

    #[must_use]
    pub fn into_phase(self) -> P {
        self.phase
    }

    #[must_use]
    pub fn into_parts(self) -> (SourceMap, P) {
        (self.sources, self.phase)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AstPhase {
    pub statements: Vec<Spanned<Stmt>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Directive(Directive),
    Declaration(Declaration),
    Fact(Fact),
    Rule(Rule),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Directive {
    Include(IncludeDirective),
    Type(TypeDirective),
    Mode(ModeDirective),
    Pragma(PragmaDirective),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludeDirective {
    pub path: Spanned<SmolStr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeDirective {
    pub name: Spanned<SmolStr>,
    pub variants: Vec<Spanned<SmolStr>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeDirective {
    pub predicate: Spanned<SmolStr>,
    pub args: Vec<Spanned<ModeArg>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeArg {
    pub direction: Spanned<ModeDirection>,
    pub ty: Spanned<TypeAst>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeDirection {
    In,
    Out,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragmaDirective {
    pub name: Spanned<SmolStr>,
    pub value: Spanned<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub kind: DeclarationKind,
    pub name: Spanned<SmolStr>,
    pub args: Vec<Spanned<DeclArg>>,
    pub attrs: Vec<Spanned<DeclAttr>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    Relation,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclArg {
    pub name: Spanned<SmolStr>,
    pub ty: Spanned<TypeAst>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclAttr {
    Extern,
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeAst {
    Named(SmolStr),
    Int,
    String,
    Bool,
    Option(Box<Spanned<TypeAst>>),
    List(Box<Spanned<TypeAst>>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub atom: Spanned<Atom>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub head: Spanned<Atom>,
    pub body: Vec<Spanned<Goal>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    pub name: Spanned<SmolStr>,
    pub terms: Vec<Spanned<Term>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Goal {
    Atom(Atom),
    Not(NotGoal),
    Constraint(Constraint),
    Aggregate(AggregateBinder),
    ChooseTopK(ChooseTopkBinder),
    Disjunction(DisjunctionGroup),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotGoal {
    pub atom: Spanned<Atom>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisjunctionGroup {
    pub branches: Vec<Vec<Spanned<Goal>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Constraint {
    Relational(RelationalConstraint),
    ArithmeticBind(ArithmeticBindConstraint),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationalConstraint {
    pub lhs: Spanned<Term>,
    pub op: Spanned<RelOp>,
    pub rhs: Spanned<Term>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArithmeticBindConstraint {
    pub target: Spanned<SmolStr>,
    pub expr: Spanned<Expr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Term(Spanned<Term>),
    UnaryNeg(Box<Spanned<Expr>>),
    Binary {
        op: Spanned<ArithOp>,
        lhs: Box<Spanned<Expr>>,
        rhs: Box<Spanned<Expr>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    Var(SmolStr),
    Wildcard,
    Int(i64),
    String(SmolStr),
    Bool(bool),
    EnumAtom {
        enum_name: Spanned<SmolStr>,
        variant_name: Spanned<SmolStr>,
    },
    None {
        turbofish: Option<Spanned<TypeAst>>,
    },
    Some(Box<Spanned<Term>>),
    List {
        items: Vec<Spanned<Term>>,
        turbofish: Option<Spanned<TypeAst>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregateBinder {
    pub out: Spanned<SmolStr>,
    pub name: Spanned<AggregateName>,
    pub projection_var: Option<Spanned<SmolStr>>,
    pub goals: Vec<Spanned<Goal>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateName {
    Count,
    CountDistinct,
    Sum,
    Min,
    Max,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChooseTopkBinder {
    pub tag: Spanned<SmolStr>,
    pub k: Spanned<Term>,
    pub group: Spanned<Term>,
    pub score_var: Spanned<SmolStr>,
    pub item_var: Spanned<SmolStr>,
    pub goals: Vec<Spanned<Goal>>,
}
