//! Stratification: the predicate dependency graph, its SCCs, and the
//! stratum assignment.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use petgraph::visit::EdgeRef;
use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};
use raql_syntax::{Goal, Spanned, SrcSpan};

use crate::diagnostics::{CompilerDiagnostic, DiagBundle};
use crate::program::TypedProgram;

#[derive(Debug, Clone)]
pub struct SccPlan {
    name: String,
    stratum: usize,
    predicates: Vec<String>,
    recursive: bool,
}

impl SccPlan {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stratum(&self) -> usize {
        self.stratum
    }

    pub fn predicates(&self) -> &[String] {
        &self.predicates
    }

    pub fn recursive(&self) -> bool {
        self.recursive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DepKind {
    Positive,
    Negative,
    Aggregate,
    Selection,
}

#[derive(Debug, Clone)]
pub(crate) struct StratificationData {
    pub(crate) strata: BTreeMap<String, usize>,
    pub(crate) sccs: Vec<SccPlan>,
}

#[derive(Debug, Clone)]
struct EdgeDiagnosticContext {
    span: SrcSpan,
    detail: String,
}

pub(crate) fn compute_strata(typed: &TypedProgram, diagnostics: &mut DiagBundle) -> StratificationData {
    let mut graph = DiGraph::<String, DepKind>::new();
    let mut nodes = BTreeMap::<String, NodeIndex>::new();
    let mut edge_contexts = BTreeMap::<usize, EdgeDiagnosticContext>::new();
    for name in typed.predicates.keys() {
        let idx = graph.add_node(name.clone());
        nodes.insert(name.clone(), idx);
    }

    for tr in &typed.rules {
        let head = tr.rule.value.head.value.name.value.to_string();
        for goal in &tr.rule.value.body {
            add_goal_edges(&head, goal, &mut graph, &nodes, &mut edge_contexts);
        }
    }

    let sccs = petgraph::algo::kosaraju_scc(&graph);
    let mut node_to_scc = BTreeMap::<NodeIndex, usize>::new();
    for (i, scc) in sccs.iter().enumerate() {
        for n in scc {
            node_to_scc.insert(*n, i);
        }
    }

    for edge in graph.edge_references() {
        let from = edge.source();
        let to = edge.target();
        if node_to_scc.get(&from) == node_to_scc.get(&to) {
            let Some(scc_idx) = node_to_scc.get(&from).copied() else {
                continue;
            };
            let kind = edge.weight();
            if *kind != DepKind::Positive {
                let cycle_path =
                    describe_cycle_path(&graph, sccs[scc_idx].as_slice(), from, to, *kind);
                let edge_context = edge_contexts.get(&edge.id().index());
                let mut diagnostic = CompilerDiagnostic::error(
                    "RAQL0401",
                    format!(
                        "non-stratifiable cycle detected: {cycle_path}; non-positive edge kind is {}",
                        dep_kind_label(*kind)
                    ),
                    edge_context.map(|ctx| ctx.span),
                );
                if let Some(ctx) = edge_context {
                    diagnostic =
                        diagnostic.with_help(format!("cycle edge context: {}", ctx.detail));
                }
                diagnostics.push(diagnostic);
            }
        }
    }

    let mut strata = BTreeMap::<String, usize>::new();
    for name in typed.predicates.keys() {
        strata.insert(name.clone(), 0);
    }

    let mut changed = true;
    let mut rounds = 0usize;
    while changed && rounds < 1024 {
        rounds += 1;
        changed = false;
        for edge in graph.edge_references() {
            let head = graph[edge.source()].clone();
            let dep = graph[edge.target()].clone();
            let dep_level = *strata.get(&dep).unwrap_or(&0);
            let required = dep_level
                + if *edge.weight() == DepKind::Positive {
                    0
                } else {
                    1
                };
            let slot = strata.entry(head).or_insert(0);
            if *slot < required {
                *slot = required;
                changed = true;
            }
        }
    }

    let mut recursive_scc = BTreeMap::<usize, bool>::new();
    let mut scc_name = BTreeMap::<usize, String>::new();
    for (idx, scc) in sccs.iter().enumerate() {
        let mut predicates = scc.iter().map(|n| graph[*n].clone()).collect::<Vec<_>>();
        predicates.sort();
        let recursive = predicates.len() > 1
            || scc.iter().any(|n| {
                graph
                    .edges(*n)
                    .any(|e| e.target() == *n && *e.weight() == DepKind::Positive)
            });
        recursive_scc.insert(idx, recursive);
        let name = if predicates.len() == 1 {
            predicates[0].clone()
        } else {
            predicates.join("+")
        };
        scc_name.insert(idx, name);
    }

    for tr in &typed.rules {
        let head = tr.rule.value.head.value.name.value.to_string();
        let Some(node) = nodes.get(&head).copied() else {
            continue;
        };
        let Some(scc_idx) = node_to_scc.get(&node).copied() else {
            continue;
        };
        if !recursive_scc.get(&scc_idx).copied().unwrap_or(false) {
            continue;
        }
        let cluster = scc_name
            .get(&scc_idx)
            .cloned()
            .unwrap_or_else(|| head.clone());
        for goal in &tr.rule.value.body {
            if let Some(kind) = recursive_forbidden_goal_kind(&goal.value) {
                diagnostics.push(CompilerDiagnostic::error(
                    "RAQL0401",
                    format!("`{kind}` is forbidden inside recursive SCC `{cluster}`"),
                    Some(goal.span),
                ));
            }
        }
    }

    let mut cond = DiGraph::<usize, ()>::new();
    let mut cond_nodes = Vec::with_capacity(sccs.len());
    for i in 0..sccs.len() {
        cond_nodes.push(cond.add_node(i));
    }
    for edge in graph.edge_references() {
        let from = edge.source();
        let to = edge.target();
        let Some(from_scc) = node_to_scc.get(&from).copied() else {
            continue;
        };
        let Some(to_scc) = node_to_scc.get(&to).copied() else {
            continue;
        };
        if from_scc == to_scc {
            continue;
        }
        // Original edge is head -> dependency; evaluation order is dependency -> head.
        cond.update_edge(cond_nodes[to_scc], cond_nodes[from_scc], ());
    }

    let topo = toposort(&cond, None).unwrap_or_default();
    let mut scc_order = BTreeMap::<usize, usize>::new();
    for (order, node) in topo.into_iter().enumerate() {
        scc_order.insert(cond[node], order);
    }

    let mut plans = Vec::with_capacity(sccs.len());
    for (idx, scc) in sccs.iter().enumerate() {
        let mut predicates = scc.iter().map(|n| graph[*n].clone()).collect::<Vec<_>>();
        predicates.sort();
        let recursive = recursive_scc.get(&idx).copied().unwrap_or(false);
        let stratum = predicates
            .iter()
            .filter_map(|p| strata.get(p).copied())
            .max()
            .unwrap_or(0);
        let name = scc_name
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| predicates.join("+"));
        plans.push((
            stratum,
            scc_order.get(&idx).copied().unwrap_or(usize::MAX),
            name.clone(),
            SccPlan {
                name,
                stratum,
                predicates,
                recursive,
            },
        ));
    }
    plans.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let sccs = plans.into_iter().map(|(_, _, _, p)| p).collect::<Vec<_>>();

    StratificationData { strata, sccs }
}

fn dep_kind_label(kind: DepKind) -> &'static str {
    match kind {
        DepKind::Positive => "positive",
        DepKind::Negative => "negative",
        DepKind::Aggregate => "aggregate",
        DepKind::Selection => "selection",
    }
}

fn describe_cycle_path(
    graph: &DiGraph<String, DepKind>,
    scc: &[NodeIndex],
    from: NodeIndex,
    to: NodeIndex,
    first_kind: DepKind,
) -> String {
    let mut parts = vec![format!(
        "`{}` -{}-> `{}`",
        graph[from],
        dep_kind_label(first_kind),
        graph[to]
    )];
    if from == to {
        return parts.join("");
    }

    let mut allowed = BTreeSet::new();
    allowed.extend(scc.iter().copied());
    if let Some(path_edges) = find_path_within_scc(graph, to, from, &allowed) {
        for (edge_from, edge_to, kind) in path_edges {
            parts.push(format!(
                "`{}` -{}-> `{}`",
                graph[edge_from],
                dep_kind_label(kind),
                graph[edge_to]
            ));
        }
    }
    parts.join(", ")
}

fn find_path_within_scc(
    graph: &DiGraph<String, DepKind>,
    start: NodeIndex,
    goal: NodeIndex,
    allowed: &BTreeSet<NodeIndex>,
) -> Option<Vec<(NodeIndex, NodeIndex, DepKind)>> {
    if start == goal {
        return Some(Vec::new());
    }

    let mut queue = VecDeque::from([start]);
    let mut seen = BTreeSet::from([start]);
    let mut parent = BTreeMap::<NodeIndex, (NodeIndex, DepKind)>::new();

    while let Some(node) = queue.pop_front() {
        for edge in graph.edges(node) {
            let next = edge.target();
            if !allowed.contains(&next) || seen.contains(&next) {
                continue;
            }
            seen.insert(next);
            parent.insert(next, (node, *edge.weight()));
            if next == goal {
                let mut cur = goal;
                let mut edges_rev = Vec::new();
                while cur != start {
                    let (prev, kind) = parent.get(&cur).copied()?;
                    edges_rev.push((prev, cur, kind));
                    cur = prev;
                }
                edges_rev.reverse();
                return Some(edges_rev);
            }
            queue.push_back(next);
        }
    }

    None
}

fn recursive_forbidden_goal_kind(goal: &Goal) -> Option<&'static str> {
    match goal {
        Goal::Aggregate(_) => Some("aggregate"),
        Goal::ChooseTopK(_) => Some("choose_topk"),
        Goal::Atom(a) => match a.name.value.as_str() {
            "witness_path" => Some("witness_path"),
            "path_hop" => Some("path_hop"),
            _ => None,
        },
        Goal::Disjunction(d) => d
            .branches
            .iter()
            .flat_map(|branch| branch.iter())
            .find_map(|g| recursive_forbidden_goal_kind(&g.value)),
        Goal::Not(_) | Goal::Constraint(_) => None,
    }
}

fn add_goal_edges(
    head: &str,
    goal: &Spanned<Goal>,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    match &goal.value {
        Goal::Atom(a) => {
            add_edge(
                head,
                a.name.value.as_str(),
                DepKind::Positive,
                graph,
                nodes,
                Some(goal.span),
                Some(format!(
                    "rule `{head}` references `{}` in body",
                    a.name.value
                )),
                edge_contexts,
            );
            add_witness_dependencies(
                head,
                a.name.value.as_str(),
                graph,
                nodes,
                Some(goal.span),
                edge_contexts,
            );
        }
        Goal::Not(n) => add_edge(
            head,
            n.atom.value.name.value.as_str(),
            DepKind::Negative,
            graph,
            nodes,
            Some(goal.span),
            Some(format!(
                "rule `{head}` has negated goal `not {}`",
                n.atom.value.name.value
            )),
            edge_contexts,
        ),
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for g in &a.goals {
                add_goal_edges_kind(head, g, DepKind::Aggregate, graph, nodes, edge_contexts);
            }
        }
        Goal::ChooseTopK(c) => {
            for g in &c.goals {
                add_goal_edges_kind(head, g, DepKind::Selection, graph, nodes, edge_contexts);
            }
        }
        Goal::Disjunction(d) => {
            for b in &d.branches {
                for g in b {
                    add_goal_edges(head, g, graph, nodes, edge_contexts);
                }
            }
        }
    }
}

fn add_goal_edges_kind(
    head: &str,
    goal: &Spanned<Goal>,
    kind: DepKind,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    match &goal.value {
        Goal::Atom(a) => {
            add_edge(
                head,
                a.name.value.as_str(),
                kind,
                graph,
                nodes,
                Some(goal.span),
                Some(format!(
                    "rule `{head}` references `{}` in {} context",
                    a.name.value,
                    dep_kind_label(kind)
                )),
                edge_contexts,
            );
            add_witness_dependencies(
                head,
                a.name.value.as_str(),
                graph,
                nodes,
                Some(goal.span),
                edge_contexts,
            );
        }
        Goal::Not(n) => add_edge(
            head,
            n.atom.value.name.value.as_str(),
            kind,
            graph,
            nodes,
            Some(goal.span),
            Some(format!(
                "rule `{head}` negates `{}` in {} context",
                n.atom.value.name.value,
                dep_kind_label(kind)
            )),
            edge_contexts,
        ),
        Goal::Constraint(_) => {}
        Goal::Aggregate(a) => {
            for g in &a.goals {
                add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
            }
        }
        Goal::ChooseTopK(c) => {
            for g in &c.goals {
                add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
            }
        }
        Goal::Disjunction(d) => {
            for b in &d.branches {
                for g in b {
                    add_goal_edges_kind(head, g, kind, graph, nodes, edge_contexts);
                }
            }
        }
    }
}

fn add_witness_dependencies(
    head: &str,
    predicate: &str,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    span: Option<SrcSpan>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    if matches!(predicate, "witness_path" | "path_hop") {
        add_edge(
            head,
            "graph_edge",
            DepKind::Selection,
            graph,
            nodes,
            span,
            Some(format!(
                "rule `{head}` uses `{predicate}`, which implies dependency on `graph_edge`"
            )),
            edge_contexts,
        );
    }
}

fn add_edge(
    head: &str,
    dep: &str,
    kind: DepKind,
    graph: &mut DiGraph<String, DepKind>,
    nodes: &BTreeMap<String, NodeIndex>,
    span: Option<SrcSpan>,
    detail: Option<String>,
    edge_contexts: &mut BTreeMap<usize, EdgeDiagnosticContext>,
) {
    let Some(from) = nodes.get(head).copied() else {
        return;
    };
    let Some(to) = nodes.get(dep).copied() else {
        return;
    };
    let edge_id = graph.add_edge(from, to, kind);
    if let (Some(span), Some(detail)) = (span, detail) {
        edge_contexts.insert(edge_id.index(), EdgeDiagnosticContext { span, detail });
    }
}
