# RAQL v0.1 Required Spec Acceptance Matrix

This matrix is a 1:1 mapping from each `(required)` section in
`docs/initial_plans/language_spec.md` to a concrete acceptance test.

Legend:
- `Covered`: mapped to an executable test.
- `Uncovered`: no concrete acceptance test currently exists.

| Required spec section | Primary acceptance test | Status |
| --- | --- | --- |
| `0.1 Compile-time errors and diagnostics (required)` | `crates/raql-compiler/src/lib.rs::reports_unknown_mode_predicate_with_exact_code` | Covered |
| `2.3 Groundness (required)` | `crates/raql-compiler/src/lib.rs::rejects_variable_fact` | Covered |
| `3.5 Type inference model (v0.1, required)` | `crates/raql-compiler/src/lib.rs::catches_ambiguous_none` | Covered |
| `5.3 Implicit schemas for derived predicates (v0.1 usability, required)` | `crates/raql-compiler/src/lib.rs::catches_ambiguous_empty_list` | Covered |
| `9.2.1 What counts as range-restricting evidence (required)` | `crates/raql-compiler/src/lib.rs::rejects_unrestricted_negation_variable` | Covered |
| `9.2.2 Non-binding constraints (required)` | `crates/raql-compiler/src/lib.rs::rejects_unrestricted_non_binding_constraint_variable` | Covered |
| `10.4 Empty input semantics (required)` | `crates/raql-engine/src/lib.rs::aggregate_empty_input_semantics_match_spec` | Covered |
| `10.5 Type restrictions (required)` | `crates/raql-compiler/src/lib.rs::{rejects_sum_with_non_int_projection,rejects_min_without_projection_form}` | Covered |
| `11.1 choose_topk Stability (required)` | `crates/raql-engine/src/lib.rs::choose_topk_is_deterministic_for_score_ties` | Covered |
| `12.2 stable_order(Term) (required total order)` | `crates/raql-engine/src/lib.rs::{relational_order_for_enums_uses_declaration_order,aggregate_min_max_for_enums_uses_declaration_order,choose_topk_ties_use_enum_declaration_order_for_items,witness_path_hop_order_uses_stable_order}` | Covered |
| `13.2 Global iteration limit (required)` | `crates/raql-engine/src/lib.rs::reports_iteration_limit_as_partial_with_notes_section` | Covered |
| `13.4 Analysis world stamp (required)` | `crates/raql-host/src/lib.rs::world_stamp_is_exposed_and_overridable` + `crates/raql-engine/src/lib.rs::world_stamp_is_injected_from_host_runtime` | Covered |
| `13.5 Runtime errors (required)` | `crates/raql-engine/src/lib.rs::{reports_division_by_zero_as_partial,runtime_error_codes_are_stable}` | Covered |
| `15. Built-in helpers (required)` | `crates/raql-engine/src/lib.rs::builtin_helpers_execute_without_host_relations` | Covered |
| `16.1 ty_app and ty_arg (required)` | `crates/raql-host-ra/tests/required_sections_16.rs::ty_app_and_ty_arg_are_zero_based_and_type_only` | Covered |
| `16.1 Wrappers (required subset)` | `crates/raql-host-ra/tests/required_sections_16.rs::wrappers_are_exposed_with_stable_indexing` | Covered |
| `16.1 Parameters and primitives (required)` | `crates/raql-host-ra/tests/required_sections_16.rs::parameters_primitives_and_unknown_are_exposed` | Covered |
| `16.2 Mapping from span to node (required, functional + optional)` | `crates/raql-host-ra/tests/required_sections_16.rs::{node_at_picks_the_most_specific_containing_node,node_at_tie_breaks_with_stable_order_node_id}` | Covered |
| `16.2 Node attributes (required, functional)` | `crates/raql-host-ra/tests/required_sections_16.rs::node_attributes_are_functional_and_optional_parent_is_supported` | Covered |
| `16.2 Enough for enclosing control structure (required via std)` | `crates/raql-host-ra/tests/required_sections_16.rs::{enclosing_control_returns_first_control_ancestor_with_distance,enclosing_control_respects_default_and_overridden_max_depth}` | Covered |

Additional required semantics tracked by this patch:

| Required semantic | Primary acceptance test | Status |
| --- | --- | --- |
| `14.4 Reservation rule (required): out_status/1 is engine-reserved` | `crates/raql-compiler/src/lib.rs::{rejects_reserved_out_status_output_declaration,rejects_reserved_out_status_fact,rejects_reserved_out_status_rule_head}` | Covered |
| `2.2 Occurs check (required within unification semantics)` | `crates/raql-compiler/src/lib.rs::occurs_check_rejects_recursive_unification` | Covered |
| `2.2 Structural unification over option/list terms in atoms` | `crates/raql-engine/src/lib.rs::atom_matching_binds_nested_some_and_list_variables` | Covered |
| `7.4 Relational '=' unification binds variables structurally` | `crates/raql-engine/src/lib.rs::equality_constraint_unifies_structural_terms_and_binds_nested_vars` | Covered |
| `6.2 Mode planner defers '=' until an operand is ground` | `crates/raql-compiler/src/lib.rs::planner_defers_eq_until_one_side_is_ground` | Covered |
| `10.2 Aggregate projection-and-dedup set semantics` | `crates/raql-engine/src/lib.rs::aggregate_count_uses_set_semantics_for_rows_and_projection` | Covered |
| `10.3 Aggregates forbidden in recursive SCC` | `crates/raql-compiler/src/lib.rs::{aggregate_recursion_cycle_is_rejected,aggregate_is_forbidden_inside_indirect_recursive_scc}` | Covered |
| `11.1 choose_topk forbidden inside recursive SCC` | `crates/raql-compiler/src/lib.rs::choose_topk_is_forbidden_inside_indirect_recursive_scc` | Covered |
| `11.2 witness_path/path_hop forbidden inside recursive SCC` | `crates/raql-compiler/src/lib.rs::witness_path_is_forbidden_inside_recursive_scc` | Covered |
| `11.2 witness_path/path_hop stratification above graph_edge` | `crates/raql-compiler/src/lib.rs::{witness_path_induces_selection_dependency_on_graph_edge,path_hop_induces_selection_dependency_on_graph_edge}` | Covered |
| `11.1 choose_topk clarification: Group must be ground` | `crates/raql-compiler/src/lib.rs::choose_topk_requires_ground_group` | Covered |
| `11.1 choose_topk candidates must bind Score/Item inside Goals` | `crates/raql-compiler/src/lib.rs::choose_topk_score_and_item_must_appear_in_goals` | Covered |
| `11.2 graph_edge/5 required host schema (Def/Def/Span columns)` | `crates/raql-compiler/src/lib.rs::{rejects_invalid_graph_edge_endpoint_or_evidence_types,accepts_required_graph_edge_schema}` | Covered |
| `10.2 aggregate projection variable must appear in Goals` | `crates/raql-compiler/src/lib.rs::aggregate_projection_variable_must_appear_in_goals` | Covered |
| `Planner failure when no runnable goal ordering exists` | `crates/raql-compiler/src/lib.rs::planner_reports_stuck_mode_with_exact_code` | Covered |
| `Stratification rejects non-monotonic cycles` | `crates/raql-compiler/src/lib.rs::stratification_rejects_negation_cycle_with_exact_code` | Covered |
| `witness_path/path_hop basic execution` | `crates/raql-engine/src/lib.rs::witness_path_and_path_hop_produce_expected_hops` | Covered |
| `16.2 enclosing_control default depth input contract (control_max_depth=32)` | `crates/raql-engine/src/lib.rs::injects_default_control_max_depth_input` | Covered |
| `Parse-level acceptance surface for witness_path/path_hop and Seq output fields` | `crates/raql-syntax/tests/parser_tests.rs::parses_witness_path_path_hop_and_seq_output_surface` | Covered |
| `16.x Engine executes RA extern predicates against repository snapshot` | `crates/raql-host-ra/tests/extern_runtime_dispatch.rs::{engine_dispatches_required_ra_extern_predicates,engine_executes_enclosing_control_with_default_depth,engine_executes_enclosing_control_with_overridden_depth_limit}` | Covered |
