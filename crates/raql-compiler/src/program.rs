//! The program representations threaded through the pipeline and the type
//! model they carry.

use std::collections::BTreeMap;

use raql_syntax::{DeclAttr, DeclarationKind, Goal, Rule, Spanned, SrcSpan, Term, TypeAst};

use crate::diagnostics::DiagBundle;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CompilerType {
    Int,
    String,
    Bool,
    Named(String),
    Option(Box<CompilerType>),
    List(Box<CompilerType>),
    Var(u32),
}

impl CompilerType {
    pub(crate) fn is_orderable(&self) -> bool {
        match self {
            Self::Int | Self::String | Self::Bool | Self::Named(_) => true,
            Self::Option(inner) | Self::List(inner) => inner.is_orderable(),
            Self::Var(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeDir {
    In,
    Out,
}

impl ModeDir {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::In => "+",
            Self::Out => "-",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::In => "input",
            Self::Out => "output",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSig {
    pub(crate) args: Vec<(ModeDir, CompilerType)>,
}

impl ModeSig {
    pub fn args(&self) -> &[(ModeDir, CompilerType)] {
        &self.args
    }
}

pub(crate) fn format_type_name(ty: &CompilerType) -> String {
    match ty {
        CompilerType::Int => "int".to_string(),
        CompilerType::String => "string".to_string(),
        CompilerType::Bool => "bool".to_string(),
        CompilerType::Named(name) => name.clone(),
        CompilerType::Option(inner) => format!("{}?", format_type_name(inner)),
        CompilerType::List(inner) => format!("[{}]", format_type_name(inner)),
        CompilerType::Var(v) => format!("_t{v}"),
    }
}

pub(crate) fn format_mode_signature(sig: &ModeSig) -> String {
    let args = sig
        .args
        .iter()
        .map(|(dir, ty)| format!("{}{}", dir.symbol(), format_type_name(ty)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("({args})")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateDecl {
    pub(crate) kind: DeclarationKind,
    pub(crate) args: Vec<CompilerType>,
    pub(crate) attrs: Vec<DeclAttr>,
    pub(crate) span: SrcSpan,
    pub(crate) inferred: bool,
    pub(crate) inferred_from: Option<PredicateUsageKind>,
}

impl PredicateDecl {
    pub fn kind(&self) -> DeclarationKind {
        self.kind
    }

    pub fn args(&self) -> &[CompilerType] {
        &self.args
    }

    pub fn attrs(&self) -> &[DeclAttr] {
        &self.attrs
    }

    pub fn span(&self) -> SrcSpan {
        self.span
    }

    pub fn inferred(&self) -> bool {
        self.inferred
    }

    pub(crate) fn is_output(&self) -> bool {
        self.attrs.contains(&DeclAttr::Output)
    }

    pub(crate) fn schema_origin(&self) -> PredicateSchemaOrigin {
        if self.inferred {
            self.inferred_from
                .map(PredicateSchemaOrigin::from_inference_source)
                .unwrap_or(PredicateSchemaOrigin::InferredFromUsage)
        } else {
            PredicateSchemaOrigin::Declaration
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    pub(crate) variants: Vec<String>,
    pub(crate) span: SrcSpan,
}

impl EnumDecl {
    pub fn variants(&self) -> &[String] {
        &self.variants
    }

    pub fn span(&self) -> SrcSpan {
        self.span
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedProgram {
    pub(crate) sources: raql_syntax::SourceMap,
    pub(crate) predicates: BTreeMap<String, PredicateDecl>,
    pub(crate) enums: BTreeMap<String, EnumDecl>,
    pub(crate) modes: BTreeMap<String, Vec<ModeSig>>,
    pub(crate) mode_spans: BTreeMap<String, Vec<SrcSpan>>,
    pub(crate) pragmas: BTreeMap<String, i64>,
    pub(crate) facts: Vec<Spanned<raql_syntax::Fact>>,
    pub(crate) rules: Vec<Spanned<Rule>>,
}

#[derive(Debug, Clone)]
pub struct TypedRule {
    pub(crate) rule: Spanned<Rule>,
    pub(crate) var_types: BTreeMap<String, CompilerType>,
}

impl TypedRule {
    pub fn rule(&self) -> &Spanned<Rule> {
        &self.rule
    }

    pub fn var_types(&self) -> &BTreeMap<String, CompilerType> {
        &self.var_types
    }

    pub fn head_predicate(&self) -> &str {
        self.rule.value.head.value.name.value.as_str()
    }

    pub fn head_terms(&self) -> &[Spanned<Term>] {
        &self.rule.value.head.value.terms
    }

    pub fn goal(&self, index: usize) -> Option<&Spanned<Goal>> {
        self.rule.value.body.get(index)
    }

    pub fn goals(&self) -> std::slice::Iter<'_, Spanned<Goal>> {
        self.rule.value.body.iter()
    }
}

#[derive(Debug, Clone)]
pub struct TypedProgram {
    pub(crate) sources: raql_syntax::SourceMap,
    pub(crate) predicates: BTreeMap<String, PredicateDecl>,
    pub(crate) enums: BTreeMap<String, EnumDecl>,
    pub(crate) modes: BTreeMap<String, Vec<ModeSig>>,
    pub(crate) pragmas: BTreeMap<String, i64>,
    pub(crate) facts: Vec<Spanned<raql_syntax::Fact>>,
    pub(crate) rules: Vec<TypedRule>,
}

pub(crate) fn parse_type_ast(
    ast: &TypeAst,
    _span: Option<SrcSpan>,
    diagnostics: &mut DiagBundle,
) -> CompilerType {
    match ast {
        TypeAst::Int => CompilerType::Int,
        TypeAst::String => CompilerType::String,
        TypeAst::Bool => CompilerType::Bool,
        TypeAst::Named(name) => CompilerType::Named(name.to_string()),
        TypeAst::Option(inner) => CompilerType::Option(Box::new(parse_type_ast(
            &inner.value,
            Some(inner.span),
            diagnostics,
        ))),
        TypeAst::List(inner) => CompilerType::List(Box::new(parse_type_ast(
            &inner.value,
            Some(inner.span),
            diagnostics,
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PredicateUsageKind {
    Fact,
    RuleHead,
    RuleBody,
    RuleBodyNegated,
}

impl PredicateUsageKind {
    pub(crate) fn can_infer_schema(self) -> bool {
        matches!(self, Self::Fact | Self::RuleHead)
    }

    pub(crate) fn usage_label(self) -> &'static str {
        match self {
            Self::Fact => "a fact",
            Self::RuleHead => "a rule head",
            Self::RuleBody => "a rule body atom",
            Self::RuleBodyNegated => "a negated rule body atom",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PredicateUsage {
    pub(crate) arity: usize,
    pub(crate) span: SrcSpan,
    pub(crate) kind: PredicateUsageKind,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PredicateSchemaOrigin {
    Declaration,
    InferredFromFact,
    InferredFromRuleHead,
    InferredFromUsage,
}

impl PredicateSchemaOrigin {
    pub(crate) fn from_inference_source(kind: PredicateUsageKind) -> Self {
        match kind {
            PredicateUsageKind::Fact => Self::InferredFromFact,
            PredicateUsageKind::RuleHead => Self::InferredFromRuleHead,
            PredicateUsageKind::RuleBody | PredicateUsageKind::RuleBodyNegated => {
                Self::InferredFromRuleHead
            }
        }
    }

    pub(crate) fn anchor_label(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::InferredFromFact => "fact",
            Self::InferredFromRuleHead => "rule head",
            Self::InferredFromUsage => "prior usage",
        }
    }
}
