//! Resolution: statements to a `ResolvedProgram`.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use raql_syntax::{AstPhase, DeclAttr, Directive, ModeDirection, Program, Spanned, SrcSpan, Stmt};

use crate::diagnostics::{CompilerDiagnostic, DiagBundle, enrich_include_stack_context};
use crate::program::{
    CompilerType, EnumDecl, ModeDir, ModeSig, PredicateDecl, ResolvedProgram, parse_type_ast,
};

pub fn resolve(program: Program<AstPhase>) -> Result<ResolvedProgram, DiagBundle> {
    let (sources, phase) = program.into_parts();
    let mut diagnostics = Vec::new();
    let mut predicates = BTreeMap::<String, PredicateDecl>::new();
    let mut enums = BTreeMap::<String, EnumDecl>::new();
    let mut modes = BTreeMap::<String, Vec<ModeSig>>::new();
    let mut mode_spans = BTreeMap::<String, Vec<SrcSpan>>::new();
    let mut pragmas = BTreeMap::<String, i64>::new();
    let mut facts = Vec::new();
    let mut rules = Vec::new();

    for stmt in &phase.statements {
        match &stmt.value {
            Stmt::Directive(dir) => match dir {
                Directive::Include(_) => {}
                Directive::Type(td) => {
                    let name = td.name.value.to_string();
                    let mut variants = Vec::new();
                    let mut seen = BTreeSet::new();
                    for v in &td.variants {
                        let variant = v.value.to_string();
                        if seen.insert(variant.clone()) {
                            variants.push(variant);
                        }
                    }
                    if enums
                        .insert(
                            name.clone(),
                            EnumDecl {
                                variants,
                                span: stmt.span,
                            },
                        )
                        .is_some()
                    {
                        diagnostics.push(CompilerDiagnostic::error(
                            "RAQL0102",
                            format!("duplicate enum `{name}`"),
                            Some(td.name.span),
                        ));
                    }
                }
                Directive::Mode(md) => {
                    let name = md.predicate.value.to_string();
                    let mut expanded = vec![Vec::<(ModeDir, CompilerType)>::new()];
                    for arg in &md.args {
                        let arg_ty = parse_type_ast(
                            &arg.value.ty.value,
                            Some(arg.value.ty.span),
                            &mut diagnostics,
                        );
                        let dirs = match arg.value.direction.value {
                            ModeDirection::In => vec![ModeDir::In],
                            ModeDirection::Out => vec![ModeDir::Out],
                            ModeDirection::Any => vec![ModeDir::In, ModeDir::Out],
                        };
                        let mut next = Vec::new();
                        for base in &expanded {
                            for d in &dirs {
                                let mut row = base.clone();
                                row.push((*d, arg_ty.clone()));
                                next.push(row);
                            }
                        }
                        expanded = next;
                    }
                    let entry = modes.entry(name).or_default();
                    let span_entry = mode_spans
                        .entry(md.predicate.value.to_string())
                        .or_default();
                    for args in expanded {
                        entry.push(ModeSig { args });
                        span_entry.push(md.predicate.span);
                    }
                }
                Directive::Pragma(p) => {
                    pragmas.insert(p.name.value.to_string(), p.value.value);
                }
            },
            Stmt::Declaration(d) => {
                let name = d.name.value.to_string();
                let mut args = Vec::new();
                for arg in &d.args {
                    args.push(parse_type_ast(
                        &arg.value.ty.value,
                        Some(arg.value.ty.span),
                        &mut diagnostics,
                    ));
                }
                let attrs = d.attrs.iter().map(|a| a.value).collect::<Vec<_>>();
                if attrs.contains(&DeclAttr::Extern) {
                    diagnostics.push(
                        CompilerDiagnostic::error(
                            "RAQL0105",
                            format!(
                                "`{name}` is declared `extern` — extern predicates come from \
                                 the catalog (SPEC §8.1), not from program text",
                            ),
                            Some(d.name.span),
                        )
                        .with_help("see `raql capabilities` for the available predicates"),
                    );
                }
                if predicates
                    .insert(
                        name.clone(),
                        PredicateDecl {
                            kind: d.kind,
                            args,
                            attrs,
                            span: d.name.span,
                            inferred: false,
                            inferred_from: None,
                        },
                    )
                    .is_some()
                {
                    diagnostics.push(CompilerDiagnostic::error(
                        "RAQL0101",
                        format!("duplicate predicate declaration `{name}`"),
                        Some(d.name.span),
                    ));
                }
            }
            Stmt::Fact(f) => facts.push(Spanned::new(stmt.span, f.clone())),
            Stmt::Rule(r) => rules.push(Spanned::new(stmt.span, r.clone())),
        }
    }

    // Catalog externs and injected engine builtins (SPEC §8.1): the
    // program receives these without declaring them, and may not
    // redeclare them.
    for (name, decl) in crate::externs::injected_declarations() {
        match predicates.entry(name) {
            Entry::Occupied(user) => {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0101",
                    format!(
                        "`{}` redeclares a catalog extern predicate — extern signatures come \
                         from the catalog (SPEC §8.1), not from program text",
                        user.key(),
                    ),
                    Some(user.get().span),
                ));
            }
            Entry::Vacant(slot) => {
                slot.insert(decl);
            }
        }
    }

    if !diagnostics.is_empty() {
        enrich_include_stack_context(&mut diagnostics, &sources);
        return Err(diagnostics);
    }

    Ok(ResolvedProgram {
        sources,
        predicates,
        enums,
        modes,
        mode_spans,
        pragmas,
        facts,
        rules,
    })
}
