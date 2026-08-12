//! Source-term evaluation and unification over the generic value model.
//!
//! The engine executes from the *source AST* (the flattened typed rules):
//! the physical plan orders goals and picks access paths, and each planned
//! goal's `source_index` leads back (through the lowering provenance) to
//! the `raql_syntax` goal whose terms carry constants, enum literals, and
//! variable names. Everything here is generic over
//! [`raql_plan::EngineValue`].

use std::collections::BTreeMap;

use raql_plan::{EngineValue, Var};
use raql_syntax::{ArithOp, Expr, Spanned, Term};

use crate::RuntimeError;

/// One rule evaluation's bindings, indexed by the logic rule's [`Var`]s.
pub(crate) type Env<V> = Vec<Option<V>>;

/// Name → logic variable map of one rule (from the logic rule's var
/// table). Source terms bind through names; planned accesses bind through
/// positions.
pub(crate) struct NameMap(BTreeMap<String, Var>);

impl NameMap {
    pub(crate) fn new(vars: &[String]) -> NameMap {
        NameMap(
            vars.iter()
                .enumerate()
                .map(|(index, name)| (name.clone(), Var(index as u32)))
                .collect(),
        )
    }

    pub(crate) fn var(&self, name: &str) -> Option<Var> {
        self.0.get(name).copied()
    }
}

pub(crate) fn env_get<V>(env: &Env<V>, var: Var) -> Option<&V> {
    env.get(var.0 as usize).and_then(Option::as_ref)
}

pub(crate) fn env_set<V>(env: &mut Env<V>, var: Var, value: V) {
    env[var.0 as usize] = Some(value);
}

/// Evaluate a ground source term against the environment. Errors on
/// unbound variables and wildcards — callers only evaluate terms the
/// planner proved bound.
pub(crate) fn eval_term<V: EngineValue>(
    term: &Spanned<Term>,
    env: &Env<V>,
    names: &NameMap,
) -> Result<V, RuntimeError> {
    match &term.value {
        Term::Var(name) => names
            .var(name)
            .and_then(|var| env_get(env, var).cloned())
            .ok_or_else(|| RuntimeError::UnboundVar(name.to_string())),
        Term::Wildcard => Err(RuntimeError::UnboundVar("_".to_string())),
        Term::Int(value) => Ok(V::int(*value)),
        Term::String(value) => Ok(V::string(value)),
        Term::Bool(value) => Ok(V::boolean(*value)),
        Term::EnumAtom { enum_name, variant_name } => {
            Ok(V::enum_tag(&enum_name.value, &variant_name.value))
        }
        Term::None { .. } => Ok(V::none()),
        Term::Some(inner) => Ok(V::some(eval_term(inner, env, names)?)),
        Term::List { items, .. } => Ok(V::list(
            items.iter().map(|item| eval_term(item, env, names)).collect::<Result<_, _>>()?,
        )),
    }
}

/// Unify a source term against a value, binding variables into the
/// environment. Shape mismatches fail the unification (set semantics),
/// they are not errors.
pub(crate) fn unify_term<V: EngineValue>(
    term: &Spanned<Term>,
    value: &V,
    env: &mut Env<V>,
    names: &NameMap,
) -> Result<bool, RuntimeError> {
    match &term.value {
        Term::Wildcard => Ok(true),
        Term::Var(name) => {
            let var = names.var(name).ok_or_else(|| RuntimeError::Internal {
                detail: format!("variable `{name}` missing from the rule's var table"),
            })?;
            match env_get(env, var) {
                Some(bound) => Ok(bound == value),
                None => {
                    env_set(env, var, value.clone());
                    Ok(true)
                }
            }
        }
        Term::Int(expected) => Ok(value.as_int() == Some(*expected)),
        Term::String(expected) => Ok(value.as_str() == Some(expected.as_str())),
        Term::Bool(expected) => Ok(value.as_bool() == Some(*expected)),
        Term::EnumAtom { enum_name, variant_name } => Ok(value.as_enum()
            == Some((enum_name.value.as_str(), variant_name.value.as_str()))),
        Term::None { .. } => Ok(value.as_option() == Some(None)),
        Term::Some(inner) => match value.as_option() {
            Some(Some(contained)) => unify_term(inner, contained, env, names),
            _ => Ok(false),
        },
        Term::List { items, .. } => match value.as_list() {
            Some(values) if values.len() == items.len() => {
                for (item, contained) in items.iter().zip(values) {
                    if !unify_term(item, contained, env, names)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
    }
}

/// Whether every variable of a term is bound (the term is evaluable).
pub(crate) fn term_ground<V: EngineValue>(
    term: &Spanned<Term>,
    env: &Env<V>,
    names: &NameMap,
) -> bool {
    match &term.value {
        Term::Var(name) => names.var(name).is_some_and(|var| env_get(env, var).is_some()),
        Term::Wildcard => false,
        Term::Int(_) | Term::String(_) | Term::Bool(_) | Term::EnumAtom { .. } => true,
        Term::None { .. } => true,
        Term::Some(inner) => term_ground(inner, env, names),
        Term::List { items, .. } => items.iter().all(|item| term_ground(item, env, names)),
    }
}

/// Evaluate an integer arithmetic expression (`:=` right-hand sides).
pub(crate) fn eval_int_expr<V: EngineValue>(
    expr: &Spanned<Expr>,
    env: &Env<V>,
    names: &NameMap,
) -> Result<i64, RuntimeError> {
    match &expr.value {
        Expr::Term(term) => {
            let value = eval_term(term, env, names)?;
            value.as_int().ok_or_else(|| RuntimeError::TypeMismatchContext {
                context: format!(
                    "arithmetic expression expected `int`, found {}",
                    value.type_tag(),
                ),
            })
        }
        Expr::UnaryNeg(inner) => eval_int_expr(inner, env, names)?
            .checked_neg()
            .ok_or(RuntimeError::Overflow),
        Expr::Binary { op, lhs, rhs } => {
            let left = eval_int_expr(lhs, env, names)?;
            let right = eval_int_expr(rhs, env, names)?;
            match op.value {
                ArithOp::Add => left.checked_add(right).ok_or(RuntimeError::Overflow),
                ArithOp::Sub => left.checked_sub(right).ok_or(RuntimeError::Overflow),
                ArithOp::Mul => left.checked_mul(right).ok_or(RuntimeError::Overflow),
                ArithOp::Div => {
                    if right == 0 {
                        Err(RuntimeError::DivisionByZero)
                    } else {
                        left.checked_div(right).ok_or(RuntimeError::Overflow)
                    }
                }
            }
        }
    }
}
