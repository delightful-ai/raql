//! Planner conformance (SPEC §9–§10): the registry-walking mode matrix,
//! the step-4 gate (seeded filters cause no enumeration), demand
//! propagation, recursion, negation rules, root semantics, and the
//! RAQL0301/0310/0311 message contracts.

use raql_plan::{
    Access, Binding, BuiltinDef, CostClass, DerivedDef, DerivedId, Goal, GoalRef, InputDef,
    OperatorId, Pattern, PhysicalPlan, PlanError, PlanOptions, PlannedGoal, Program, Root, Rule,
    Term, Var, plan, v0_catalog,
};

fn v(i: u32) -> Term {
    Term::Var(Var(i))
}

fn extern_goal(name: &str, args: Vec<Term>) -> Goal {
    Goal::positive(GoalRef::Extern(name.to_owned()), args)
}

fn rule(vars: &[&str], head: Vec<Term>, body: Vec<Goal>) -> Rule {
    Rule { vars: vars.iter().map(|s| (*s).to_owned()).collect(), head, body }
}

/// Wrap derived defs plus one ad-hoc rule body as a program with a single
/// arity-0 root — the lowering shape for a lone query (SPEC §9.2).
fn program_with(
    mut derived: Vec<DerivedDef>,
    inputs: Vec<InputDef>,
    builtins: Vec<BuiltinDef>,
    vars: &[&str],
    body: Vec<Goal>,
) -> Program {
    let query = DerivedId(derived.len());
    derived.push(DerivedDef {
        name: "query".to_owned(),
        arity: 0,
        declared_modes: None,
        rules: vec![rule(vars, Vec::new(), body)],
    });
    Program {
        derived,
        inputs,
        builtins,
        roots: vec![Root { predicate: query, pattern: Pattern::new(Vec::new()) }],
    }
}

fn query_program(vars: &[&str], body: Vec<Goal>) -> Program {
    program_with(Vec::new(), Vec::new(), Vec::new(), vars, body)
}

/// `target_def`-style single-column input relation.
fn input(name: &str, arity: usize) -> InputDef {
    InputDef { name: name.to_owned(), arity }
}

/// The planned goals of the single ad-hoc root's rule.
fn root_goals<'p>(physical: &'p PhysicalPlan, program: &Program) -> &'p [PlannedGoal] {
    assert_eq!(program.roots.len(), 1);
    let spec = physical.root_specialization(&program.roots[0]);
    assert_eq!(spec.rules.len(), 1);
    &spec.rules[0].goals
}

fn planned_operators(program: &Program) -> Vec<OperatorId> {
    let physical = plan(program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let mut operators = Vec::new();
    for spec in &physical.specializations {
        for planned in &spec.rules {
            for goal in &planned.goals {
                if let Access::Extern { mode, .. } = &goal.access {
                    operators.push(mode.operator);
                }
            }
        }
    }
    operators
}

/// Registry-walking mode matrix (SPEC §16.2 at plan level): every declared
/// mode of every predicate, exercised with exactly its `+` set bound, is
/// the mode the planner chooses; disabled predicates are plan-time
/// capability errors.
#[test]
fn registry_mode_matrix() {
    let catalog = v0_catalog();
    for predicate in catalog.entries() {
        if predicate.is_disabled() {
            let args = (0..predicate.arity() as u32).map(v).collect();
            let names: Vec<&str> = predicate.args.iter().map(|a| a.name).collect();
            let program = query_program(&names, vec![extern_goal(predicate.name, args)]);
            let error = plan(&program, &catalog, PlanOptions::default()).unwrap_err();
            assert_eq!(error.code(), "RAQL0302", "{}: {error}", predicate.name);
            continue;
        }
        for mode in predicate.modes {
            let bound_vars: Vec<Term> = mode
                .pattern
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == Binding::Bound)
                .map(|(i, _)| v(i as u32))
                .collect();
            let mut body = Vec::new();
            let mut inputs = Vec::new();
            if !bound_vars.is_empty() {
                inputs.push(input("seed", bound_vars.len()));
                body.push(Goal::positive(GoalRef::Input(raql_plan::InputId(0)), bound_vars));
            }
            let goal_index = body.len();
            body.push(extern_goal(
                predicate.name,
                (0..predicate.arity() as u32).map(v).collect(),
            ));
            let names: Vec<&str> = predicate.args.iter().map(|a| a.name).collect();
            let program = program_with(Vec::new(), inputs, Vec::new(), &names, body);
            let physical = plan(&program, &catalog, PlanOptions::default())
                .unwrap_or_else(|e| panic!("{} mode {:?}: {e}", predicate.name, mode.pattern));
            let planned = root_goals(&physical, &program)
                .iter()
                .find(|g| g.source_index == goal_index)
                .expect("goal planned");
            let Access::Extern { mode: chosen, .. } = &planned.access else {
                panic!("extern access expected");
            };
            assert_eq!(
                chosen.operator, mode.operator,
                "{}: mode {:?} must be chosen under exactly its own binding",
                predicate.name, mode.pattern,
            );
        }
    }
}

/// The step-4 gate (SPEC §17.2): a seeded `is_fn`-style filter causes no
/// `def` enumeration — the §9 demand machinery closes the old
/// filter-forces-scan leak.
#[test]
fn seeded_is_fn_causes_no_enumeration() {
    let is_fn = DerivedDef {
        name: "is_fn".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![
            rule(&["D"], vec![v(0)], vec![extern_goal("def_kind", vec![v(0), Term::Const])]),
            rule(&["D"], vec![v(0)], vec![extern_goal("def_kind", vec![v(0), Term::Const])]),
        ],
    };
    let program = program_with(
        vec![is_fn],
        vec![input("target_def", 1)],
        Vec::new(),
        &["D", "N"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
            Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)]),
            extern_goal("def_name", vec![v(0), v(1)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    assert!(physical.scans.is_empty(), "seeded filter must not scan: {:?}", physical.scans);
    let operators = planned_operators(&program);
    assert!(
        !operators.contains(&OperatorId::DefsScan) && !operators.contains(&OperatorId::FnDefsScan),
        "no enumeration operator may appear: {operators:?}",
    );
    // Two specializations: the query root, and is_fn demanded under (+) only.
    assert_eq!(physical.specializations.len(), 2);
    let is_fn_spec = physical
        .specializations
        .iter()
        .find(|spec| spec.predicate == DerivedId(0))
        .expect("is_fn specialized");
    assert_eq!(is_fn_spec.pattern, Pattern::from(vec![true]));
    assert!(!is_fn_spec.is_scan);
    assert_eq!(physical.max_cost, CostClass::C0);
    // And the same program plans under --no-scan.
    plan(&program, &v0_catalog(), PlanOptions { deny_scans: true }).expect("scan-free plan");
}

/// Explicit enumeration is visible (§8.5) and refused under `--no-scan`
/// with RAQL0310.
#[test]
fn explicit_scans_are_visible_and_deniable() {
    let program = query_program(
        &["F", "C", "S", "K"],
        vec![
            extern_goal("fn_def", vec![v(0)]),
            extern_goal("callee", vec![v(0), v(1), v(2), v(3)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    assert_eq!(physical.scans.len(), 1);
    assert_eq!(physical.scans[0].predicate, "fn_def");
    assert_eq!(physical.scans[0].cost, CostClass::C4);
    assert_eq!(physical.max_cost, CostClass::C4);
    // The scan is placed only because nothing else is placeable, and the
    // bound direction runs after it.
    let goals = root_goals(&physical, &program);
    assert_eq!(goals[0].source_index, 0);
    assert_eq!(goals[1].source_index, 1);

    let error = plan(&program, &v0_catalog(), PlanOptions { deny_scans: true }).unwrap_err();
    assert_eq!(
        error.to_string(),
        "error[RAQL0310]: plan requires the scan `fn_def` [C4], denied by --no-scan",
    );
}

/// The SPEC §10.3 message contract, verbatim shape.
#[test]
fn raql0301_message_contract() {
    let program = query_program(
        &["Caller", "Callee", "Site", "Disp"],
        vec![extern_goal("call_edge", vec![v(0), v(1), v(2), v(3)])],
    );
    let error = plan(&program, &v0_catalog(), PlanOptions { deny_scans: true }).unwrap_err();
    assert_eq!(error.code(), "RAQL0301");
    let message = error.to_string();
    let expected = "\
error[RAQL0301]: no satisfiable access path for `call_edge(Caller, Callee, Site, Disp)`
  bound here: (none)
  supported: call_edge(+Caller, -, -, -)   by-caller  [C1]
             call_edge(-, +Callee, -, -)   by-callee  [C2]
             call_edge(-, -, -, -)         scan  [C4]  (denied: --no-scan)
  fix: bind Caller or Callee first (e.g. via def_name/def_at), or allow scans";
    assert_eq!(message, expected);
}

/// RAQL0301 reports the bindings at the failure point and the minimal
/// unlock set.
#[test]
fn raql0301_reports_bound_arguments() {
    let program = program_with(
        Vec::new(),
        vec![input("target_def", 1)],
        Vec::new(),
        &["F", "X", "S", "K"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(1)]),
            extern_goal("caller", vec![v(0), v(1), v(2), v(3)]),
        ],
    );
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    let PlanError::UnsatisfiableModes(goal) = &error else {
        panic!("expected RAQL0301, got {error}");
    };
    assert_eq!(goal.goal, "caller(F, X, S, K)");
    assert_eq!(goal.bound, vec!["X".to_owned()]);
    assert_eq!(goal.unlock_sets, vec![vec!["F".to_owned()]]);
    assert_eq!(goal.seed_hint, vec!["def_name", "def_at"]);
    assert!(!goal.scans_denied);
    let message = error.to_string();
    assert!(message.contains("bound here: X"), "{message}");
    assert!(message.contains("fix: bind F first (e.g. via def_name/def_at)"), "{message}");
}

/// Demand propagation memoizes per (predicate, pattern) — SPEC §9.2.
#[test]
fn demand_is_memoized_per_pattern() {
    let is_fn = DerivedDef {
        name: "is_fn".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![rule(
            &["D"],
            vec![v(0)],
            vec![extern_goal("def_kind", vec![v(0), Term::Const])],
        )],
    };
    let program = program_with(
        vec![is_fn],
        vec![input("target_def", 1)],
        Vec::new(),
        &["D"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
            Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)]),
            Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let is_fn_specs = physical
        .specializations
        .iter()
        .filter(|spec| spec.predicate == DerivedId(0))
        .count();
    assert_eq!(is_fn_specs, 1);
}

/// Recursive SCC mode inference (SPEC §9.1 greatest fixpoint): transitive
/// reachability over call edges supports both seeded directions, and each
/// demanded direction plans onto the matching physical operator.
#[test]
fn recursion_supports_both_seeded_directions() {
    let reach = DerivedDef {
        name: "reach".to_owned(),
        arity: 2,
        declared_modes: None,
        rules: vec![
            rule(
                &["X", "Y", "S", "K"],
                vec![v(0), v(1)],
                vec![extern_goal("call_edge", vec![v(0), v(1), v(2), v(3)])],
            ),
            rule(
                &["X", "Y", "Z", "S", "K"],
                vec![v(0), v(1)],
                vec![
                    extern_goal("call_edge", vec![v(0), v(2), v(3), v(4)]),
                    Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(2), v(1)]),
                ],
            ),
        ],
    };
    let build = |seed_position: usize| {
        program_with(
            vec![reach.clone()],
            vec![input("target_def", 1)],
            Vec::new(),
            &["A", "B"],
            vec![
                Goal::positive(
                    GoalRef::Input(raql_plan::InputId(0)),
                    vec![v(seed_position as u32)],
                ),
                Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0), v(1)]),
            ],
        )
    };

    // Seed the caller side: outgoing composition (by-caller), no scans.
    let forward = plan(&build(0), &v0_catalog(), PlanOptions::default()).expect("plans");
    assert!(forward.scans.is_empty());
    let reach_specs: Vec<&Pattern> = forward
        .specializations
        .iter()
        .filter(|spec| spec.predicate == DerivedId(0))
        .map(|spec| &spec.pattern)
        .collect();
    assert_eq!(reach_specs, vec![&Pattern::from(vec![true, false])]);
    let forward_ops = planned_operators(&build(0));
    assert!(forward_ops.contains(&OperatorId::CallEdgesByCaller));
    assert!(!forward_ops.contains(&OperatorId::CallEdgesByCallee));

    // Seed the callee side: reference-search composition (by-callee).
    let backward_ops = planned_operators(&build(1));
    assert!(backward_ops.contains(&OperatorId::CallEdgesByCallee));
    assert!(!backward_ops.contains(&OperatorId::CallEdgesByCaller));
}

/// An unseeded derived *call* is a scan of a derived predicate (§9.2):
/// legal, reported with the scans, cost = max of its leaves.
#[test]
fn derived_scan_is_visible() {
    let def_allowed = DerivedDef {
        name: "def_allowed".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![rule(
            &["D", "S"],
            vec![v(0)],
            vec![
                extern_goal("def", vec![v(0)]),
                extern_goal("def_span", vec![v(0), v(1)]),
                extern_goal("span_allowed", vec![v(1)]),
            ],
        )],
    };
    let program = program_with(
        vec![def_allowed],
        Vec::new(),
        Vec::new(),
        &["D"],
        vec![Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)])],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let scans: Vec<(&str, CostClass)> = physical
        .scans
        .iter()
        .map(|scan| (scan.predicate.as_str(), scan.cost))
        .collect();
    assert_eq!(scans, vec![("def_allowed", CostClass::C4), ("def", CostClass::C4)]);
    let def_allowed_spec = physical
        .specializations
        .iter()
        .find(|spec| spec.predicate == DerivedId(0))
        .expect("def_allowed specialized");
    assert!(def_allowed_spec.is_scan);
    assert_eq!(physical.max_cost, CostClass::C4);

    // Under --no-scan the leaf enumeration is the refusal (RAQL0310).
    let error = plan(&program, &v0_catalog(), PlanOptions { deny_scans: true }).unwrap_err();
    assert_eq!(error.code(), "RAQL0310");
    assert!(error.to_string().contains("`def`"), "{error}");
}

/// A demand root is the request's own demand, not a call: view outputs
/// demanded unseeded are not reported as scans, while unseeded *calls*
/// inside their bodies still are (§9.2).
#[test]
fn roots_are_not_scans() {
    // target(T) :- def_name(T, "load_and_plan").   (internally seeded)
    let target = DerivedDef {
        name: "target".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![rule(
            &["T"],
            vec![v(0)],
            vec![extern_goal("def_name", vec![v(0), Term::Const])],
        )],
    };
    // report(C, N) :- target(T), callee(T, C, _, _), def_name(C, N).
    let report = DerivedDef {
        name: "report".to_owned(),
        arity: 2,
        declared_modes: None,
        rules: vec![rule(
            &["C", "N", "T", "S", "K"],
            vec![v(0), v(1)],
            vec![
                Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(2)]),
                extern_goal("callee", vec![v(2), v(0), v(3), v(4)]),
                extern_goal("def_name", vec![v(0), v(1)]),
            ],
        )],
    };
    let program = Program {
        derived: vec![target, report],
        inputs: Vec::new(),
        builtins: Vec::new(),
        roots: vec![Root {
            predicate: DerivedId(1),
            pattern: Pattern::from(vec![false, false]),
        }],
    };
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");

    // The root's own full-extent evaluation is not a scan...
    let report_spec = physical.root_specialization(&program.roots[0]);
    assert!(!report_spec.is_scan);
    assert!(!physical.scans.iter().any(|scan| scan.predicate == "report"));
    // ...but the unseeded `target(T)` call inside its body is (§9.2),
    // costed at its leaves (the C2 name seed), and the whole plan stays
    // legal under --no-scan because nothing bottoms out in an extern scan.
    assert_eq!(physical.scans.len(), 1);
    assert_eq!(physical.scans[0].predicate, "target");
    assert_eq!(physical.scans[0].cost, CostClass::C2);
    plan(&program, &v0_catalog(), PlanOptions { deny_scans: true }).expect("scan-free leaves");
}

/// Declared `.mode` assertions are the public contract (SPEC §9.1), for
/// call sites and demand roots alike.
#[test]
fn declared_modes_are_the_contract() {
    let is_fn = DerivedDef {
        name: "is_fn".to_owned(),
        arity: 1,
        declared_modes: Some(vec![vec![Binding::Bound]]),
        rules: vec![rule(
            &["D"],
            vec![v(0)],
            vec![extern_goal("def_kind", vec![v(0), Term::Const])],
        )],
    };
    // Calling under the empty pattern is refused even though `def_kind`
    // alone would leave it merely unsupported: (+) is the whole contract.
    let program = program_with(
        vec![is_fn.clone()],
        Vec::new(),
        Vec::new(),
        &["D"],
        vec![Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)])],
    );
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0301");
    assert!(error.to_string().contains("is_fn(+D)"), "{error}");

    // A root demand outside the contract is refused the same way.
    let program = Program {
        derived: vec![is_fn],
        inputs: Vec::new(),
        builtins: Vec::new(),
        roots: vec![Root { predicate: DerivedId(0), pattern: Pattern::from(vec![false]) }],
    };
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0301");
    assert!(error.to_string().contains("is_fn(-)"), "{error}");
    assert!(error.to_string().contains("is_fn(+)"), "{error}");
}

/// A declared mode the rules cannot honor is RAQL0303.
#[test]
fn undeliverable_declared_mode_is_an_error() {
    let q = DerivedDef {
        name: "q".to_owned(),
        arity: 1,
        declared_modes: Some(vec![vec![Binding::Free]]),
        rules: vec![rule(
            &["D", "C", "S", "K"],
            vec![v(0)],
            vec![extern_goal("caller", vec![v(0), v(1), v(2), v(3)])],
        )],
    };
    let program = program_with(vec![q], Vec::new(), Vec::new(), &[], Vec::new());
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0303");
    assert_eq!(
        error.to_string(),
        "error[RAQL0303]: declared mode `q(-)` is not inferable from its rules (SPEC §9.1)",
    );
}

/// Negation rules (SPEC §8.5): negated goals run after their shared
/// variables are bound, bind nothing, and must be scan-free.
#[test]
fn negation_is_ordered_and_scan_free() {
    // `target_def(D), not is_public(D)` — the filter runs second.
    let program = program_with(
        Vec::new(),
        vec![input("target_def", 1)],
        Vec::new(),
        &["D"],
        vec![
            Goal::negated(GoalRef::Extern("is_public".to_owned()), vec![v(0)]),
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let goals = root_goals(&physical, &program);
    assert_eq!(goals[0].source_index, 1, "input binds first");
    assert_eq!(goals[1].source_index, 0);
    assert!(goals[1].negated);

    // A pure-scan extern under `not` has no non-scan mode: RAQL0301.
    let program = program_with(
        Vec::new(),
        vec![input("target_def", 1)],
        Vec::new(),
        &["D"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
            Goal::negated(GoalRef::Extern("fn_def".to_owned()), vec![v(0)]),
        ],
    );
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0301");

    // A negated derived goal whose specialization reaches a scan: RAQL0311.
    let p = DerivedDef {
        name: "p".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![rule(&["D"], vec![v(0)], vec![extern_goal("def", vec![v(0)])])],
    };
    let program = program_with(
        vec![p],
        vec![input("target_def", 1)],
        Vec::new(),
        &["D"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
            Goal::negated(GoalRef::Derived(DerivedId(0)), vec![v(0)]),
        ],
    );
    let error = plan(&program, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0311");
    assert!(error.to_string().contains("p(+)"), "{error}");
}

/// Builtins run once any accepted pattern's inputs are bound; `=`-style
/// builtins accept several patterns.
#[test]
fn builtins_wait_for_their_inputs() {
    let upper = BuiltinDef {
        name: "upper".to_owned(),
        patterns: vec![vec![Binding::Bound, Binding::Free]],
    };
    let program = program_with(
        Vec::new(),
        vec![input("seed", 1)],
        vec![upper.clone()],
        &["X", "Y"],
        vec![
            Goal::positive(GoalRef::Builtin(raql_plan::BuiltinId(0)), vec![v(0), v(1)]),
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let goals = root_goals(&physical, &program);
    assert_eq!(goals[0].source_index, 1, "seed binds X first");
    assert_eq!(goals[1].source_index, 0);

    let unseeded = program_with(
        Vec::new(),
        Vec::new(),
        vec![upper],
        &["X", "Y"],
        vec![Goal::positive(GoalRef::Builtin(raql_plan::BuiltinId(0)), vec![v(0), v(1)])],
    );
    let error = plan(&unseeded, &v0_catalog(), PlanOptions::default()).unwrap_err();
    assert_eq!(error.code(), "RAQL0301");
    assert!(error.to_string().contains("upper(+X, -)"), "{error}");

    // An either-side builtin (`X = Y`) runs as soon as one side is bound.
    let eq = BuiltinDef {
        name: "eq".to_owned(),
        patterns: vec![
            vec![Binding::Bound, Binding::Free],
            vec![Binding::Free, Binding::Bound],
        ],
    };
    let program = program_with(
        Vec::new(),
        vec![input("seed", 1)],
        vec![eq],
        &["X", "Y", "N"],
        vec![
            Goal::positive(GoalRef::Builtin(raql_plan::BuiltinId(0)), vec![v(0), v(1)]),
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(1)]),
            extern_goal("def_name", vec![v(0), v(2)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let goals = root_goals(&physical, &program);
    assert_eq!(goals[0].source_index, 1, "seed binds Y first");
    assert_eq!(goals[1].source_index, 0, "eq runs on its (-, +) pattern");
    assert_eq!(goals[2].source_index, 2, "def_name projects off the eq-bound X");
}

/// Same program + same catalog ⇒ identical plan (SPEC §10.1).
#[test]
fn planning_is_deterministic() {
    let program = query_program(
        &["F", "C", "S", "K", "N"],
        vec![
            extern_goal("fn_def", vec![v(0)]),
            extern_goal("callee", vec![v(0), v(1), v(2), v(3)]),
            extern_goal("def_name", vec![v(1), v(4)]),
        ],
    );
    let first = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let second = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    assert_eq!(first, second);
}

/// The §10.4 explain rendering, end to end: roots first, then demanded
/// specializations; single-rule bodies inline without a rule header.
#[test]
fn explain_renders_the_plan() {
    let is_fn = DerivedDef {
        name: "is_fn".to_owned(),
        arity: 1,
        declared_modes: None,
        rules: vec![rule(
            &["D"],
            vec![v(0)],
            vec![extern_goal("def_kind", vec![v(0), Term::Const])],
        )],
    };
    let program = program_with(
        vec![is_fn],
        vec![input("target_def", 1)],
        Vec::new(),
        &["D", "N"],
        vec![
            Goal::positive(GoalRef::Input(raql_plan::InputId(0)), vec![v(0)]),
            Goal::positive(GoalRef::Derived(DerivedId(0)), vec![v(0)]),
            extern_goal("def_name", vec![v(0), v(1)]),
        ],
    );
    let physical = plan(&program, &v0_catalog(), PlanOptions::default()).expect("plans");
    let expected = "\
root query() [C0]:
  1. target_def(D)  ->  input target_def
  2. def_name(D, N)  ->  def_name/name-of-def (+,-) keyed [C0] via [hir name projection]
  3. is_fn(D)  ->  derived is_fn(+)
specialization is_fn(+) [C0]:
  1. def_kind(D, <const>)  ->  def_kind/kind-of-def (+,-) keyed [C0] via [value variant projection]
scans: (none)
max cost: C0
";
    assert_eq!(physical.explain(&program), expected);
}
