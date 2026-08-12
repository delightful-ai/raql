//! Definition at a file position (SPEC §8.6 `def_at(+,+,-)`, cost C1).
//!
//! Goto-definition shape: token at offset → macro descent → identifier
//! classification, keeping `hir` identities (SPEC §6.3). Positions that
//! resolve to no definition (whitespace, literals, locals, unresolved names)
//! have no rows — `ra_resolved`, caveat `unresolved_positions_absent`.

use std::collections::HashSet;

use ide_db::defs::IdentClass;
use ide_db::helpers::pick_best_token;
use syntax::{AstNode, SyntaxKind};

use crate::def::Def;
use crate::operators::OperatorError;
use crate::snapshot::Attached;
use crate::value::{Position, Value};

pub(super) fn classify_at_position(
    att: &Attached<'_>,
    file: ide_db::FileId,
    pos: Position,
) -> Result<Vec<Vec<Value>>, OperatorError> {
    let db = att.db();
    let line_index = ide_db::line_index(db, file);
    let Some(offset) = line_index.offset(ide_db::line_index::LineCol {
        line: pos.line,
        col: pos.col,
    }) else {
        // Out-of-range positions have no rows.
        return Ok(Vec::new());
    };

    let sema = hir::Semantics::new(db);
    let source_file = sema.parse_guess_edition(file);
    let Some(token) = pick_best_token(
        source_file.syntax().token_at_offset(offset),
        |kind| match kind {
            SyntaxKind::IDENT
            | SyntaxKind::SELF_KW
            | SyntaxKind::SUPER_KW
            | SyntaxKind::CRATE_KW
            | SyntaxKind::SELF_TYPE_KW => 2,
            kind if kind.is_trivia() => 0,
            _ => 1,
        },
    ) else {
        return Ok(Vec::new());
    };

    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for descended in sema.descend_into_macros_exact(token) {
        let Some(parent) = descended.parent() else {
            continue;
        };
        let Some(ident_class) = IdentClass::classify_node(&sema, &parent) else {
            continue;
        };
        for definition in ident_class.definitions_no_ops() {
            let Some(def) = Def::from_ide_definition(db, definition) else {
                continue;
            };
            if seen.insert(def) {
                rows.push(vec![
                    Value::File(file),
                    Value::Position(pos),
                    Value::Def(def),
                ]);
            }
        }
    }
    Ok(rows)
}
