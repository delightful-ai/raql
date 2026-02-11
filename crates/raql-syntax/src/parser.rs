use crate::ast::{
    AggregateBinder, AggregateName, ArithOp, ArithmeticBindConstraint, AstPhase, Atom,
    ChooseTopkBinder, Constraint, DeclArg, DeclAttr, Declaration, DeclarationKind, Directive,
    DisjunctionGroup, Expr, Fact, Goal, IncludeDirective, ModeArg, ModeDirection, ModeDirective,
    NotGoal, PragmaDirective, RelOp, RelationalConstraint, Rule, Stmt, Term, TypeAst,
    TypeDirective,
};
use crate::diag::{Diagnostic, DiagnosticLabel};
use crate::lexer::{Token, TokenKind, lex};
use crate::source::{RaqlFileId, Spanned, SrcSpan};
use camino::Utf8PathBuf;
use smol_str::SmolStr;
use text_size::{TextRange, TextSize};

pub(crate) fn parse_source(
    file: RaqlFileId,
    source: &str,
    include_stack: &[Utf8PathBuf],
) -> (AstPhase, Vec<Diagnostic>) {
    let (tokens, mut diagnostics) = lex(file, source, include_stack);
    let mut parser = Parser {
        file,
        tokens,
        index: 0,
        diagnostics: Vec::new(),
        include_stack: include_stack.to_vec(),
    };

    let statements = parser.parse_statements();
    diagnostics.extend(parser.diagnostics);

    (AstPhase { statements }, diagnostics)
}

struct Parser {
    file: RaqlFileId,
    tokens: Vec<Token>,
    index: usize,
    diagnostics: Vec<Diagnostic>,
    include_stack: Vec<Utf8PathBuf>,
}

impl Parser {
    fn parse_statements(&mut self) -> Vec<Spanned<Stmt>> {
        let mut statements = Vec::new();
        while !self.at_eof() {
            let checkpoint = self.index;
            if let Some(stmt) = self.parse_statement() {
                statements.push(stmt);
                continue;
            }
            self.recover_to_statement_boundary(checkpoint);
        }
        statements
    }

    fn parse_statement(&mut self) -> Option<Spanned<Stmt>> {
        if self.at(Punct::Dot) {
            self.parse_dotted_statement()
        } else {
            self.parse_fact_or_rule_statement()
        }
    }

    fn parse_dotted_statement(&mut self) -> Option<Spanned<Stmt>> {
        let start = self.expect_punct(
            Punct::Dot,
            "expected `.` to start a directive or declaration",
        )?;
        let keyword = self.expect_ident("expected directive/declaration keyword after `.`")?;
        let keyword_name = keyword.value.as_str();

        match keyword_name {
            "include" => {
                let path = self.expect_string(
                    "expected include path string literal (example: `.include \"file.raql\".`); hint: wrap the include path in double quotes before the closing `.`",
                )?;
                let end = self.expect_punct(Punct::Dot, "expected `.` after include directive")?;
                let span = self.span_from_ranges(start.range, end.range);
                let directive = IncludeDirective { path };
                Some(Spanned::new(
                    span,
                    Stmt::Directive(Directive::Include(directive)),
                ))
            }
            "type" => self.parse_type_directive(start.range),
            "mode" => self.parse_mode_directive(start.range),
            "pragma" => self.parse_pragma_directive(start.range),
            "decl" => self.parse_declaration(start.range, DeclarationKind::Relation),
            "func" => self.parse_declaration(start.range, DeclarationKind::Function),
            _ => {
                self.error_at(
                    keyword.span.range,
                    format!(
                        "unknown dotted statement `.{keyword_name}`; expected one of include/type/mode/pragma/decl/func"
                    ),
                );
                None
            }
        }
    }

    fn parse_type_directive(&mut self, start_range: TextRange) -> Option<Spanned<Stmt>> {
        let name = self.expect_ident("expected type name after `.type`")?;
        self.expect_punct(Punct::Eq, "expected `=` in `.type` directive")?;
        self.expect_punct(Punct::LBrace, "expected `{` in `.type` directive")?;

        let variants =
            self.parse_comma_separated(|parser| parser.expect_ident("expected enum variant name"))?;

        self.expect_punct(Punct::RBrace, "expected `}` after enum variants")?;
        let end = self.expect_punct(Punct::Dot, "expected `.` after `.type` directive")?;

        let span = self.span_from_ranges(start_range, end.range);
        let directive = TypeDirective { name, variants };
        Some(Spanned::new(
            span,
            Stmt::Directive(Directive::Type(directive)),
        ))
    }

    fn parse_mode_directive(&mut self, start_range: TextRange) -> Option<Spanned<Stmt>> {
        let predicate = self.expect_ident("expected predicate name in `.mode`")?;
        self.expect_punct(Punct::LParen, "expected `(` after mode predicate")?;

        let mut args = Vec::new();
        if !self.at(Punct::RParen) {
            args = self.parse_comma_separated(|parser| {
                let direction_token = parser.bump()?;
                let direction = match direction_token.kind {
                    TokenKind::Plus => ModeDirection::In,
                    TokenKind::Minus => ModeDirection::Out,
                    TokenKind::Question => ModeDirection::Any,
                    _ => {
                        parser.error_at(
                            direction_token.range,
                            "expected one of `+`, `-`, or `?` in mode argument",
                        );
                        return None;
                    }
                };
                let direction =
                    Spanned::new(parser.span_from_range(direction_token.range), direction);
                let ty = parser.parse_type()?;
                let span = direction.span.merge(ty.span);
                Some(Spanned::new(span, ModeArg { direction, ty }))
            })?;
        }

        self.expect_punct(Punct::RParen, "expected `)` after mode arguments")?;
        let end = self.expect_punct(Punct::Dot, "expected `.` after `.mode` directive")?;
        let span = self.span_from_ranges(start_range, end.range);

        let directive = ModeDirective { predicate, args };
        Some(Spanned::new(
            span,
            Stmt::Directive(Directive::Mode(directive)),
        ))
    }

    fn parse_pragma_directive(&mut self, start_range: TextRange) -> Option<Spanned<Stmt>> {
        let name = self.expect_ident("expected pragma name")?;
        self.expect_punct(Punct::Eq, "expected `=` in pragma directive")?;
        let value = self.expect_signed_int("expected integer literal in pragma directive")?;
        let end = self.expect_punct(Punct::Dot, "expected `.` after pragma directive")?;

        let span = self.span_from_ranges(start_range, end.range);
        let directive = PragmaDirective { name, value };
        Some(Spanned::new(
            span,
            Stmt::Directive(Directive::Pragma(directive)),
        ))
    }

    fn parse_declaration(
        &mut self,
        start_range: TextRange,
        kind: DeclarationKind,
    ) -> Option<Spanned<Stmt>> {
        let name = self.expect_ident("expected predicate name")?;
        self.expect_punct(Punct::LParen, "expected `(` in declaration")?;

        let mut args = Vec::new();
        if !self.at(Punct::RParen) {
            args = self.parse_comma_separated(|parser| {
                let arg_name = parser.expect_ident("expected argument name")?;
                parser.expect_punct(Punct::Colon, "expected `:` in declaration argument")?;
                let arg_ty = parser.parse_type()?;
                let span = arg_name.span.merge(arg_ty.span);
                Some(Spanned::new(
                    span,
                    DeclArg {
                        name: arg_name,
                        ty: arg_ty,
                    },
                ))
            })?;
        }

        self.expect_punct(Punct::RParen, "expected `)` after declaration arguments")?;

        let mut attrs = Vec::new();
        loop {
            let Some(ident) = self.peek_ident_name() else {
                break;
            };
            let attr = match ident {
                "extern" => DeclAttr::Extern,
                "input" => DeclAttr::Input,
                "output" => DeclAttr::Output,
                _ => break,
            };
            let token = self.bump()?;
            attrs.push(Spanned::new(self.span_from_range(token.range), attr));
        }

        let end = self.expect_punct(Punct::Dot, "expected `.` after declaration")?;
        let span = self.span_from_ranges(start_range, end.range);

        let declaration = Declaration {
            kind,
            name,
            args,
            attrs,
        };
        Some(Spanned::new(span, Stmt::Declaration(declaration)))
    }

    fn parse_fact_or_rule_statement(&mut self) -> Option<Spanned<Stmt>> {
        let atom = self.parse_atom()?;
        if self.consume_punct(Punct::RuleArrow).is_some() {
            let body = self.parse_goal_list_until(&[Punct::Dot], "rule body")?;
            let end = self.expect_punct(Punct::Dot, "expected `.` after rule")?;
            let span = self.span_from_ranges(atom.span.range, end.range);
            let rule = Rule { head: atom, body };
            Some(Spanned::new(span, Stmt::Rule(rule)))
        } else {
            let end = self.expect_punct(Punct::Dot, "expected `.` after fact")?;
            let span = self.span_from_ranges(atom.span.range, end.range);
            let fact = Fact { atom };
            Some(Spanned::new(span, Stmt::Fact(fact)))
        }
    }

    fn parse_goal(&mut self) -> Option<Spanned<Goal>> {
        if self.at_keyword("not") {
            return self.parse_not_goal();
        }
        if self.at_keyword("choose_topk") {
            return self.parse_choose_topk_goal();
        }
        if self.at(Punct::LParen) {
            return self.parse_disjunction_goal();
        }
        if self.looks_like_aggregate() {
            return self.parse_aggregate_goal();
        }
        if self.looks_like_atom_goal() {
            let atom = self.parse_atom()?;
            let span = atom.span;
            return Some(Spanned::new(span, Goal::Atom(atom.value)));
        }
        self.parse_constraint_goal()
    }

    fn parse_not_goal(&mut self) -> Option<Spanned<Goal>> {
        let not_token = self.bump()?;
        debug_assert!(matches!(&not_token.kind, TokenKind::Ident(name) if name.as_str() == "not"));
        let atom = self.parse_atom()?;
        let span = self.span_from_range(not_token.range).merge(atom.span);
        let goal = Goal::Not(NotGoal { atom });
        Some(Spanned::new(span, goal))
    }

    fn parse_disjunction_goal(&mut self) -> Option<Spanned<Goal>> {
        let start = self.expect_punct(Punct::LParen, "expected `(`")?;
        let first_branch =
            self.parse_goal_list_until(&[Punct::Semicolon, Punct::RParen], "disjunction branch")?;
        if self.consume_punct(Punct::Semicolon).is_none() {
            self.error_at(
                self.current_range(),
                "disjunction group must contain `;` between branches (example: `(edge(A, B) ; edge(B, A))`)",
            );
            return None;
        }

        let mut branches = vec![first_branch];
        loop {
            let branch = self
                .parse_goal_list_until(&[Punct::Semicolon, Punct::RParen], "disjunction branch")?;
            branches.push(branch);
            if self.consume_punct(Punct::Semicolon).is_some() {
                continue;
            }
            break;
        }

        let end = self.expect_punct(Punct::RParen, "expected `)` after disjunction group")?;
        let span = self.span_from_ranges(start.range, end.range);
        let group = DisjunctionGroup { branches };
        Some(Spanned::new(span, Goal::Disjunction(group)))
    }

    fn parse_constraint_goal(&mut self) -> Option<Spanned<Goal>> {
        if self.looks_like_arith_binding() {
            let target = self.parse_variable_name("expected variable before `:=`")?;
            self.expect_punct(Punct::ColonEq, "expected `:=`")?;
            let expr = self.parse_expr()?;
            let span = target.span.merge(expr.span);
            let constraint = Constraint::ArithmeticBind(ArithmeticBindConstraint { target, expr });
            return Some(Spanned::new(span, Goal::Constraint(constraint)));
        }

        let lhs = self.parse_term()?;
        let op_token = self.bump()?;
        let relop = match op_token.kind {
            TokenKind::Eq => RelOp::Eq,
            TokenKind::NotEq => RelOp::NotEq,
            TokenKind::Lt => RelOp::Lt,
            TokenKind::LtEq => RelOp::LtEq,
            TokenKind::Gt => RelOp::Gt,
            TokenKind::GtEq => RelOp::GtEq,
            _ => {
                self.error_at(
                    op_token.range,
                    "expected relational operator (`=`, `!=`, `<`, `<=`, `>`, `>=`)",
                );
                return None;
            }
        };
        let op = Spanned::new(self.span_from_range(op_token.range), relop);
        let rhs = self.parse_term()?;
        let span = lhs.span.merge(rhs.span);
        let constraint = Constraint::Relational(RelationalConstraint { lhs, op, rhs });
        Some(Spanned::new(span, Goal::Constraint(constraint)))
    }

    fn parse_aggregate_goal(&mut self) -> Option<Spanned<Goal>> {
        let out = self.parse_variable_name("expected output variable in aggregate binder")?;
        self.expect_punct(Punct::Eq, "expected `=` in aggregate binder")?;

        let agg_name = self.expect_ident("expected aggregate name")?;
        let agg_name_text = agg_name.value.as_str();
        let agg_name_kind = match agg_name_text {
            "count" => AggregateName::Count,
            "count_distinct" => AggregateName::CountDistinct,
            "sum" => AggregateName::Sum,
            "min" => AggregateName::Min,
            "max" => AggregateName::Max,
            _ => {
                self.error_at(
                    agg_name.span.range,
                    format!(
                        "expected aggregate name (`count`, `count_distinct`, `sum`, `min`, `max`), found `{agg_name_text}`"
                    ),
                );
                return None;
            }
        };
        let name = Spanned::new(agg_name.span, agg_name_kind);

        self.expect_punct(Punct::LParen, "expected `(` after aggregate name")?;

        let projection_var = if self.looks_like_projection_prefix() {
            let var = self.parse_variable_name("expected projection variable")?;
            self.expect_punct(Punct::Colon, "expected `:` after projection variable")?;
            Some(var)
        } else {
            None
        };

        let goals = self.parse_goal_list_until(&[Punct::RParen], "aggregate goals")?;
        let end = self.expect_punct(Punct::RParen, "expected `)` to close aggregate binder")?;

        let span = out.span.merge(self.span_from_range(end.range));
        let binder = AggregateBinder {
            out,
            name,
            projection_var,
            goals,
        };
        Some(Spanned::new(span, Goal::Aggregate(binder)))
    }

    fn parse_choose_topk_goal(&mut self) -> Option<Spanned<Goal>> {
        let start_token = self.bump()?;
        debug_assert!(
            matches!(&start_token.kind, TokenKind::Ident(name) if name.as_str() == "choose_topk")
        );
        self.expect_punct(Punct::LParen, "expected `(` after `choose_topk`")?;

        let tag = self.expect_string("expected string tag as first argument to `choose_topk`")?;
        self.expect_punct(Punct::Comma, "expected `,` after choose_topk tag")?;
        let k = self.parse_term()?;
        self.expect_punct(Punct::Comma, "expected `,` after choose_topk K term")?;
        let group = self.parse_term()?;
        self.expect_punct(Punct::Comma, "expected `,` after choose_topk group term")?;
        let score_var = self.parse_variable_name("expected score variable in choose_topk")?;
        self.expect_punct(
            Punct::Comma,
            "expected `,` after choose_topk score variable",
        )?;
        let item_var = self.parse_variable_name("expected item variable in choose_topk")?;
        self.expect_punct(
            Punct::Colon,
            "expected `:` before choose_topk candidate goals",
        )?;

        let goals = self.parse_goal_list_until(&[Punct::RParen], "choose_topk goals")?;
        let end = self.expect_punct(Punct::RParen, "expected `)` after choose_topk goals")?;

        let span = self.span_from_ranges(start_token.range, end.range);
        let binder = ChooseTopkBinder {
            tag,
            k,
            group,
            score_var,
            item_var,
            goals,
        };
        Some(Spanned::new(span, Goal::ChooseTopK(binder)))
    }

    fn parse_goal_list_until(
        &mut self,
        terminators: &[Punct],
        context: &str,
    ) -> Option<Vec<Spanned<Goal>>> {
        if self.at_any(terminators) {
            self.error_at(self.current_range(), self.empty_goal_list_message(context));
            return None;
        }

        let mut goals = Vec::new();
        goals.push(self.parse_goal()?);
        while self.consume_punct(Punct::Comma).is_some() {
            if self.at_any(terminators) {
                self.error_at(
                    self.current_range(),
                    self.trailing_goal_after_comma_message(context),
                );
                return None;
            }
            goals.push(self.parse_goal()?);
        }
        Some(goals)
    }

    fn empty_goal_list_message(&self, context: &str) -> String {
        match context {
            "choose_topk goals" => "expected at least one choose_topk candidate goal after `Item :` (example: `choose_topk(\"tag\", 3, Group, Score, Item : edge(Group, Item))`)".to_string(),
            _ => format!("expected at least one goal in {context}; add a goal like `edge(A, B)`"),
        }
    }

    fn trailing_goal_after_comma_message(&self, context: &str) -> String {
        match context {
            "choose_topk goals" => "trailing `,` in choose_topk candidate goals after `Item :`; remove the trailing comma or add another goal (e.g. `edge(Group, Item), Score := Item + 1`)".to_string(),
            _ => format!(
                "trailing `,` in {context}; remove the trailing comma or add another goal"
            ),
        }
    }

    fn parse_expr(&mut self) -> Option<Spanned<Expr>> {
        self.parse_expr_add()
    }

    fn parse_expr_add(&mut self) -> Option<Spanned<Expr>> {
        let mut lhs = self.parse_expr_mul()?;
        loop {
            let op = if self.at(Punct::Plus) {
                Some(ArithOp::Add)
            } else if self.at(Punct::Minus) {
                Some(ArithOp::Sub)
            } else {
                None
            };
            let Some(op_kind) = op else {
                break;
            };
            let op_token = self.bump()?;
            let rhs = self.parse_expr_mul()?;
            let op_span = Spanned::new(self.span_from_range(op_token.range), op_kind);
            let span = lhs.span.merge(rhs.span);
            lhs = Spanned::new(
                span,
                Expr::Binary {
                    op: op_span,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            );
        }
        Some(lhs)
    }

    fn parse_expr_mul(&mut self) -> Option<Spanned<Expr>> {
        let mut lhs = self.parse_expr_unary()?;
        loop {
            let op = if self.at(Punct::Star) {
                Some(ArithOp::Mul)
            } else if self.at(Punct::Slash) {
                Some(ArithOp::Div)
            } else {
                None
            };
            let Some(op_kind) = op else {
                break;
            };
            let op_token = self.bump()?;
            let rhs = self.parse_expr_unary()?;
            let op_span = Spanned::new(self.span_from_range(op_token.range), op_kind);
            let span = lhs.span.merge(rhs.span);
            lhs = Spanned::new(
                span,
                Expr::Binary {
                    op: op_span,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            );
        }
        Some(lhs)
    }

    fn parse_expr_unary(&mut self) -> Option<Spanned<Expr>> {
        if self.consume_punct(Punct::Minus).is_some() {
            let minus_span = self.span_from_range(self.previous_range());
            let inner = self.parse_expr_unary()?;
            let span = minus_span.merge(inner.span);
            return Some(Spanned::new(span, Expr::UnaryNeg(Box::new(inner))));
        }

        if self.consume_punct(Punct::LParen).is_some() {
            let start = self.previous_range();
            let expr = self.parse_expr()?;
            let end =
                self.expect_punct(Punct::RParen, "expected `)` after parenthesized expression")?;
            let span = self.span_from_ranges(start, end.range);
            return Some(Spanned::new(span, expr.value));
        }

        let term = self.parse_term()?;
        let span = term.span;
        Some(Spanned::new(span, Expr::Term(term)))
    }

    fn parse_atom(&mut self) -> Option<Spanned<Atom>> {
        let name = self.expect_ident("expected predicate name")?;
        self.expect_punct(Punct::LParen, "expected `(` after predicate name")?;

        let mut terms = Vec::new();
        if !self.at(Punct::RParen) {
            terms = self.parse_comma_separated(|parser| parser.parse_term())?;
        }

        if !self.at(Punct::RParen) && self.looks_like_term_start() {
            self.error_at(
                self.current_range(),
                "expected `,` between atom arguments before the next term (example: `edge(A, B)`)",
            );
            return None;
        }

        let end = self.expect_punct(
            Punct::RParen,
            "expected `)` after atom arguments (if another argument follows, separate it with `,`, e.g. `edge(A, B)`)",
        )?;
        let span = self.span_from_ranges(name.span.range, end.range);
        Some(Spanned::new(span, Atom { name, terms }))
    }

    fn parse_term(&mut self) -> Option<Spanned<Term>> {
        if self.consume_punct(Punct::Minus).is_some() {
            let minus = self.previous_range();
            let Some(Token {
                kind: TokenKind::Int(value),
                range,
            }) = self.bump()
            else {
                self.error_at(minus, "expected integer literal after `-` in term");
                return None;
            };
            let span = self.span_from_ranges(minus, range);
            return Some(Spanned::new(span, Term::Int(-value)));
        }

        let token = self.bump()?;
        match token.kind {
            TokenKind::Int(value) => Some(Spanned::new(
                self.span_from_range(token.range),
                Term::Int(value),
            )),
            TokenKind::Str(value) => Some(Spanned::new(
                self.span_from_range(token.range),
                Term::String(value),
            )),
            TokenKind::LBracket => self.parse_list_term_from_lbracket(token.range),
            TokenKind::Ident(name) => self.parse_ident_term(name, token.range),
            _ => {
                self.error_at(
                    token.range,
                    "expected term; accepted forms: variable (`X`), wildcard (`_`), literal (`1`, `\"s\"`, `true`), enum atom (`Mode::Variant`), `none`/`some(...)`, or list (`[X, 1]`)",
                );
                None
            }
        }
    }

    fn parse_ident_term(&mut self, name: SmolStr, range: TextRange) -> Option<Spanned<Term>> {
        match name.as_str() {
            "true" => return Some(Spanned::new(self.span_from_range(range), Term::Bool(true))),
            "false" => return Some(Spanned::new(self.span_from_range(range), Term::Bool(false))),
            "none" => {
                let turbofish = self.parse_optional_turbofish()?;
                let span = if let Some(ref ann) = turbofish {
                    self.span_from_range(range).merge(ann.span)
                } else {
                    self.span_from_range(range)
                };
                return Some(Spanned::new(span, Term::None { turbofish }));
            }
            "some" => {
                self.expect_punct(Punct::LParen, "expected `(` after `some`")?;
                let inner = self.parse_term()?;
                let end = self.expect_punct(Punct::RParen, "expected `)` after `some(...)`")?;
                let span = self.span_from_ranges(range, end.range);
                return Some(Spanned::new(span, Term::Some(Box::new(inner))));
            }
            _ => {}
        }

        if self.consume_punct(Punct::ColonColon).is_some() {
            let variant = self.expect_ident("expected enum variant name after `::`")?;
            let enum_name = Spanned::new(self.span_from_range(range), name);
            let span = enum_name.span.merge(variant.span);
            return Some(Spanned::new(
                span,
                Term::EnumAtom {
                    enum_name,
                    variant_name: variant,
                },
            ));
        }

        if is_variable_name(name.as_str()) {
            let span = self.span_from_range(range);
            if name.as_str() == "_" {
                return Some(Spanned::new(span, Term::Wildcard));
            }
            return Some(Spanned::new(span, Term::Var(name)));
        }

        self.error_at(
            range,
            "expected variable, literal, enum atom, `none`, `some(...)`, or list term",
        );
        None
    }

    fn parse_list_term_from_lbracket(&mut self, lbracket: TextRange) -> Option<Spanned<Term>> {
        let mut items = Vec::new();
        if !self.at(Punct::RBracket) {
            loop {
                let term = self.parse_term()?;
                items.push(term);
                if self.consume_punct(Punct::Comma).is_some() {
                    continue;
                }
                break;
            }
        }

        if !self.at(Punct::RBracket) && self.looks_like_term_start() {
            self.error_at(
                self.current_range(),
                "expected `,` between list items before the next term (example: `[A, B]`)",
            );
            return None;
        }

        let end_bracket = self.expect_punct(
            Punct::RBracket,
            "expected `]` after list literal (if another item follows, separate it with `,`, e.g. `[A, B]`)",
        )?;
        let turbofish = self.parse_optional_turbofish()?;

        let end_span = turbofish
            .as_ref()
            .map_or(self.span_from_range(end_bracket.range), |ann| ann.span);
        let span = self.span_from_range(lbracket).merge(end_span);

        Some(Spanned::new(span, Term::List { items, turbofish }))
    }

    fn parse_optional_turbofish(&mut self) -> Option<Option<Spanned<TypeAst>>> {
        if !self.at(Punct::ColonColon) {
            return Some(None);
        }

        let start = self.bump()?;
        self.expect_punct(Punct::Lt, "expected `<` after `::` in type annotation")?;
        let ty = self.parse_type()?;
        let end = self.expect_punct(Punct::Gt, "expected `>` to close type annotation")?;
        let span = self.span_from_ranges(start.range, end.range);

        Some(Some(Spanned::new(span, ty.value)))
    }

    fn parse_comma_separated<T>(
        &mut self,
        mut parse_item: impl FnMut(&mut Self) -> Option<T>,
    ) -> Option<Vec<T>> {
        let mut items = Vec::new();
        loop {
            items.push(parse_item(self)?);
            if self.consume_punct(Punct::Comma).is_none() {
                break;
            }
        }
        Some(items)
    }

    fn parse_type(&mut self) -> Option<Spanned<TypeAst>> {
        let name = self.expect_ident("expected type")?;
        let name_span = name.span;

        match name.value.as_str() {
            "int" => Some(Spanned::new(name_span, TypeAst::Int)),
            "string" => Some(Spanned::new(name_span, TypeAst::String)),
            "bool" => Some(Spanned::new(name_span, TypeAst::Bool)),
            "option" => self.parse_parameterized_type(
                name_span,
                "expected `<...>` after `option` type constructor (example: `option<int>`)",
                "expected `>` to close `option<...>` (example: `option<int>`)",
                |inner| TypeAst::Option(inner),
            ),
            "list" => self.parse_parameterized_type(
                name_span,
                "expected `<...>` after `list` type constructor (example: `list<int>`)",
                "expected `>` to close `list<...>` (example: `list<int>`)",
                |inner| TypeAst::List(inner),
            ),
            _ => Some(Spanned::new(name_span, TypeAst::Named(name.value))),
        }
    }

    fn parse_parameterized_type(
        &mut self,
        name_span: SrcSpan,
        open_err: &str,
        close_err: &str,
        ctor: impl FnOnce(Box<Spanned<TypeAst>>) -> TypeAst,
    ) -> Option<Spanned<TypeAst>> {
        if self.consume_punct(Punct::Lt).is_none() {
            self.error_at(name_span.range, open_err);
            return None;
        }
        let inner = self.parse_type()?;
        if self.consume_punct(Punct::Gt).is_none() {
            self.error_at(name_span.merge(inner.span).range, close_err);
            return None;
        }
        let span = name_span.merge(self.span_from_range(self.previous_range()));
        Some(Spanned::new(span, ctor(Box::new(inner))))
    }

    fn parse_variable_name(&mut self, message: &str) -> Option<Spanned<SmolStr>> {
        let name = self.expect_ident(message)?;
        if !is_variable_name(name.value.as_str()) {
            self.error_at(
                name.span.range,
                format!(
                    "expected variable name (must start with `_` or an uppercase letter, e.g. `X` or `_Tmp`), found `{}`",
                    name.value
                ),
            );
            return None;
        }
        Some(name)
    }

    fn expect_signed_int(&mut self, message: &str) -> Option<Spanned<i64>> {
        let negative_span = self
            .consume_punct(Punct::Minus)
            .map(|_| self.previous_range());
        let is_negative = negative_span.is_some();
        let token = self.bump()?;
        let token_range = token.range;
        let TokenKind::Int(value) = token.kind else {
            self.error_at(negative_span.unwrap_or(token_range), message);
            return None;
        };
        let span = negative_span
            .map(|minus| self.span_from_ranges(minus, token_range))
            .unwrap_or_else(|| self.span_from_range(token_range));
        let signed = if is_negative { -value } else { value };
        Some(Spanned::new(span, signed))
    }

    fn expect_ident(&mut self, message: &str) -> Option<Spanned<SmolStr>> {
        let token = self.bump()?;
        let TokenKind::Ident(name) = token.kind else {
            self.error_at(token.range, message);
            return None;
        };
        Some(Spanned::new(self.span_from_range(token.range), name))
    }

    fn expect_string(&mut self, message: &str) -> Option<Spanned<SmolStr>> {
        let token = self.bump()?;
        let TokenKind::Str(value) = token.kind else {
            self.error_at(token.range, message);
            return None;
        };
        Some(Spanned::new(self.span_from_range(token.range), value))
    }

    fn expect_punct(&mut self, punct: Punct, message: &str) -> Option<Token> {
        if self.at(punct) {
            return self.bump();
        }
        self.error_at(self.current_range(), message);
        None
    }

    fn consume_punct(&mut self, punct: Punct) -> Option<Token> {
        if self.at(punct) { self.bump() } else { None }
    }

    fn at(&self, punct: Punct) -> bool {
        matches_punct(self.peek_kind(), punct)
    }

    fn at_any(&self, puncts: &[Punct]) -> bool {
        puncts.iter().any(|punct| self.at(*punct))
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        matches!(self.peek_kind(), TokenKind::Ident(name) if name.as_str() == keyword)
    }

    fn peek_ident_name(&self) -> Option<&str> {
        self.peek_kind().ident_name()
    }

    fn looks_like_atom_goal(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Ident(_)) && self.nth_matches_punct(1, Punct::LParen)
    }

    fn looks_like_aggregate(&self) -> bool {
        let Some(TokenKind::Ident(name)) = self.nth_kind(0) else {
            return false;
        };
        if !is_variable_name(name.as_str()) {
            return false;
        }
        if !self.nth_matches_punct(1, Punct::Eq) {
            return false;
        }
        let Some(TokenKind::Ident(agg_name)) = self.nth_kind(2) else {
            return false;
        };
        if !is_aggregate_name(agg_name.as_str()) {
            return false;
        }
        self.nth_matches_punct(3, Punct::LParen)
    }

    fn looks_like_projection_prefix(&self) -> bool {
        let Some(TokenKind::Ident(name)) = self.nth_kind(0) else {
            return false;
        };
        is_variable_name(name.as_str()) && self.nth_matches_punct(1, Punct::Colon)
    }

    fn looks_like_arith_binding(&self) -> bool {
        let Some(TokenKind::Ident(name)) = self.nth_kind(0) else {
            return false;
        };
        is_variable_name(name.as_str()) && self.nth_matches_punct(1, Punct::ColonEq)
    }

    fn looks_like_term_start(&self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Int(_)
                | TokenKind::Str(_)
                | TokenKind::LBracket
                | TokenKind::Ident(_)
                | TokenKind::Minus
        )
    }

    fn recover_to_statement_boundary(&mut self, checkpoint: usize) {
        let mut skipped_start: Option<TextRange> = None;
        let mut skipped_end: Option<TextRange> = None;
        let mut skipped_count = 0usize;
        let mut found_statement_boundary = false;

        if self.index == checkpoint {
            if let Some(token) = self.bump() {
                skipped_start = Some(token.range);
                skipped_end = Some(token.range);
                skipped_count = 1;
            }
        }

        while !self.at_eof() {
            if self.at(Punct::Dot) {
                self.bump();
                found_statement_boundary = true;
                break;
            }
            let Some(token) = self.bump() else {
                break;
            };
            if skipped_start.is_none() {
                skipped_start = Some(token.range);
            }
            skipped_end = Some(token.range);
            skipped_count += 1;
        }

        if let (Some(start), Some(end)) = (skipped_start, skipped_end) {
            self.emit_recovery_diagnostic(start, end, skipped_count, found_statement_boundary);
        }
    }

    fn emit_recovery_diagnostic(
        &mut self,
        start: TextRange,
        end: TextRange,
        skipped_count: usize,
        found_statement_boundary: bool,
    ) {
        let span = self.span_from_ranges(start, end);
        let boundary_note = if found_statement_boundary {
            "parser resumed after the next `.` statement boundary"
        } else {
            "parser reached end of input before finding the next `.` statement boundary"
        };
        let likely_cause_note = "likely cause: missing `.` after the previous statement or malformed syntax in this region";

        self.diagnostics.push(
            Diagnostic::parse(
                format!(
                    "parser recovery skipped {skipped_count} token(s) while synchronizing to the next statement boundary; likely cause: missing `.` after the previous statement"
                ),
                Some(DiagnosticLabel::new(
                    span,
                    "skipped during parser recovery",
                )),
            )
            .with_note(boundary_note)
            .with_note(likely_cause_note)
            .with_include_stack(self.include_stack.clone()),
        );
    }

    fn error_at(&mut self, range: TextRange, message: impl Into<String>) {
        let message = message.into();
        let label = self.parse_error_label(range, &message);
        let span = self.span_from_range(range);
        self.diagnostics.push(
            Diagnostic::parse(message, Some(DiagnosticLabel::new(span, label)))
                .with_include_stack(self.include_stack.clone()),
        );
    }

    fn parse_error_label(&self, range: TextRange, message: &str) -> String {
        let actual = self
            .token_context_for_range(range)
            .unwrap_or_else(|| "end of input".to_string());
        if let Some(expected) = Self::expected_clause_from_message(message) {
            if expected.contains("found `") {
                expected
            } else {
                format!("{expected}; found {actual}")
            }
        } else {
            format!("unexpected {actual} in this position")
        }
    }

    fn expected_clause_from_message(message: &str) -> Option<String> {
        let expected_index = message.find("expected ")?;
        let mut clause = message[expected_index..].trim().to_string();

        if let Some(index) = clause.find(';') {
            clause.truncate(index);
        }
        for marker in [" (example:", " (if "] {
            if let Some(index) = clause.find(marker) {
                clause.truncate(index);
            }
        }

        let clause = clause.trim().to_string();
        if clause.is_empty() {
            None
        } else {
            Some(clause)
        }
    }

    fn token_context_for_range(&self, range: TextRange) -> Option<String> {
        self.token_kind_for_range(range)
            .map(Self::token_description)
    }

    fn token_kind_for_range(&self, range: TextRange) -> Option<&TokenKind> {
        if let Some(token) = self.tokens.iter().find(|token| token.range == range) {
            return Some(&token.kind);
        }

        if let Some(token) = self
            .tokens
            .iter()
            .find(|token| token.range.start() < range.end() && range.start() < token.range.end())
        {
            return Some(&token.kind);
        }

        self.tokens
            .get(self.index)
            .map(|token| &token.kind)
            .or_else(|| self.tokens.last().map(|token| &token.kind))
    }

    fn token_description(kind: &TokenKind) -> String {
        match kind {
            TokenKind::Ident(name) => format!("identifier `{name}`"),
            TokenKind::Int(value) => format!("integer literal `{value}`"),
            TokenKind::Str(value) => {
                let escaped = value.as_str().escape_debug().to_string();
                format!("string literal \"{escaped}\"")
            }
            TokenKind::Dot => "token `.`".to_string(),
            TokenKind::Comma => "token `,`".to_string(),
            TokenKind::Colon => "token `:`".to_string(),
            TokenKind::ColonColon => "token `::`".to_string(),
            TokenKind::ColonEq => "token `:=`".to_string(),
            TokenKind::Semicolon => "token `;`".to_string(),
            TokenKind::LParen => "token `(`".to_string(),
            TokenKind::RParen => "token `)`".to_string(),
            TokenKind::LBrace => "token `{`".to_string(),
            TokenKind::RBrace => "token `}`".to_string(),
            TokenKind::LBracket => "token `[`".to_string(),
            TokenKind::RBracket => "token `]`".to_string(),
            TokenKind::Lt => "token `<`".to_string(),
            TokenKind::LtEq => "token `<=`".to_string(),
            TokenKind::Gt => "token `>`".to_string(),
            TokenKind::GtEq => "token `>=`".to_string(),
            TokenKind::Eq => "token `=`".to_string(),
            TokenKind::NotEq => "token `!=`".to_string(),
            TokenKind::Plus => "token `+`".to_string(),
            TokenKind::Minus => "token `-`".to_string(),
            TokenKind::Star => "token `*`".to_string(),
            TokenKind::Slash => "token `/`".to_string(),
            TokenKind::Question => "token `?`".to_string(),
            TokenKind::RuleArrow => "token `:-`".to_string(),
            TokenKind::Eof => "end of input".to_string(),
        }
    }

    fn span_from_range(&self, range: TextRange) -> SrcSpan {
        SrcSpan::from_range(self.file, range)
    }

    fn span_from_ranges(&self, start: TextRange, end: TextRange) -> SrcSpan {
        SrcSpan::from_range(self.file, TextRange::new(start.start(), end.end()))
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.tokens[self.index.min(self.tokens.len().saturating_sub(1))].kind
    }

    fn nth_kind(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens
            .get(self.index + offset)
            .map(|token| &token.kind)
    }

    fn nth_matches_punct(&self, offset: usize, punct: Punct) -> bool {
        self.nth_kind(offset)
            .is_some_and(|kind| matches_punct(kind, punct))
    }

    fn current_range(&self) -> TextRange {
        self.tokens.get(self.index).map_or(
            TextRange::new(TextSize::from(0), TextSize::from(0)),
            |token| token.range,
        )
    }

    fn previous_range(&self) -> TextRange {
        if self.index == 0 {
            TextRange::new(TextSize::from(0), TextSize::from(0))
        } else {
            self.tokens[self.index - 1].range
        }
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.index).cloned()?;
        self.index += 1;
        Some(token)
    }
}

#[derive(Clone, Copy)]
enum Punct {
    Dot,
    Comma,
    Colon,
    ColonColon,
    ColonEq,
    Semicolon,
    LParen,
    RParen,
    LBrace,
    RBrace,
    RBracket,
    Lt,
    Gt,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    RuleArrow,
}

fn matches_punct(kind: &TokenKind, punct: Punct) -> bool {
    match punct {
        Punct::Dot => matches!(kind, TokenKind::Dot),
        Punct::Comma => matches!(kind, TokenKind::Comma),
        Punct::Colon => matches!(kind, TokenKind::Colon),
        Punct::ColonColon => matches!(kind, TokenKind::ColonColon),
        Punct::ColonEq => matches!(kind, TokenKind::ColonEq),
        Punct::Semicolon => matches!(kind, TokenKind::Semicolon),
        Punct::LParen => matches!(kind, TokenKind::LParen),
        Punct::RParen => matches!(kind, TokenKind::RParen),
        Punct::LBrace => matches!(kind, TokenKind::LBrace),
        Punct::RBrace => matches!(kind, TokenKind::RBrace),
        Punct::RBracket => matches!(kind, TokenKind::RBracket),
        Punct::Lt => matches!(kind, TokenKind::Lt),
        Punct::Gt => matches!(kind, TokenKind::Gt),
        Punct::Eq => matches!(kind, TokenKind::Eq),
        Punct::Plus => matches!(kind, TokenKind::Plus),
        Punct::Minus => matches!(kind, TokenKind::Minus),
        Punct::Star => matches!(kind, TokenKind::Star),
        Punct::Slash => matches!(kind, TokenKind::Slash),
        Punct::RuleArrow => matches!(kind, TokenKind::RuleArrow),
    }
}

fn is_variable_name(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    first == '_' || first.is_ascii_uppercase()
}

fn is_aggregate_name(name: &str) -> bool {
    matches!(name, "count" | "count_distinct" | "sum" | "min" | "max")
}
