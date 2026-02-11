use camino::Utf8PathBuf;
use raql_syntax::{DiagnosticKind, IncludeLoader};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> Utf8PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "raql_syntax_{label}_{}_{}",
        std::process::id(),
        stamp
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    Utf8PathBuf::from_path_buf(dir).expect("utf8 temp path")
}

#[test]
fn loads_includes_with_stack_context() {
    let root = temp_dir("include_ok");
    let entry = root.join("main.raql");
    let leaf = root.join("leaf.raql");

    fs::write(
        entry.as_std_path(),
        ".include \"leaf.raql\".\n.decl p(X: int) input.\np(1).\n",
    )
    .expect("write entry");
    fs::write(leaf.as_std_path(), ".decl q(X: int) input.\nq(2).\n").expect("write leaf");

    let loader = IncludeLoader::new(vec![]);
    let program = loader
        .load_program(entry.as_path())
        .expect("program should load");

    assert_eq!(program.sources().file_count(), 2);
    assert!(program.phase().statements.len() >= 4);
}

#[test]
fn reports_missing_include() {
    let root = temp_dir("include_missing");
    let entry = root.join("main.raql");
    let middle = root.join("middle.raql");
    fs::write(entry.as_std_path(), ".include \"middle.raql\".\n").expect("write entry");
    fs::write(middle.as_std_path(), ".include \"missing.raql\".\n").expect("write middle");

    let loader = IncludeLoader::new(vec![]);
    let diagnostics = loader
        .load_program(entry.as_path())
        .expect_err("loader should fail for missing include");

    assert!(
        diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::MissingInclude)
    );
    let missing = diagnostics
        .iter()
        .find(|d| d.kind == DiagnosticKind::MissingInclude)
        .expect("missing include diagnostic");
    assert!(
        missing
            .message
            .contains("searched include locations in order")
    );
    assert!(
        missing
            .notes
            .iter()
            .any(|note| note.contains("search order:"))
    );
    assert!(
        missing
            .notes
            .iter()
            .any(|note| note.contains("corrective action:"))
    );
    assert!(
        missing
            .notes
            .iter()
            .any(|note| note.contains("tried candidate"))
    );
    let primary = missing.primary.as_ref().expect("missing include primary");
    assert!(primary.message.contains("include leg 2/2"));
    assert!(primary.message.contains("middle.raql"));
    assert!(
        missing
            .secondary
            .iter()
            .any(|label| label.message.contains("include leg 1/2")
                && label.message.contains("main.raql")
                && label.message.contains("middle.raql"))
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| !d.include_stack.is_empty() || d.message.contains("include"))
    );
}

#[test]
fn reports_include_cycle() {
    let root = temp_dir("include_cycle");
    let a = root.join("a.raql");
    let b = root.join("b.raql");

    fs::write(a.as_std_path(), ".include \"b.raql\".\n").expect("write a");
    fs::write(b.as_std_path(), ".include \"a.raql\".\n").expect("write b");

    let loader = IncludeLoader::new(vec![]);
    let diagnostics = loader
        .load_program(a.as_path())
        .expect_err("loader should fail for include cycle");

    assert!(
        diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::IncludeCycle)
    );
    let cycle = diagnostics
        .iter()
        .find(|d| d.kind == DiagnosticKind::IncludeCycle)
        .expect("include cycle diagnostic");
    assert!(cycle.message.contains("while expanding `.include`"));
    assert!(
        cycle
            .message
            .contains("remove one `.include` edge or move shared declarations into a third file")
    );
    assert!(
        cycle
            .notes
            .iter()
            .any(|note| note.contains("expanded depth-first"))
    );
    assert!(
        cycle
            .notes
            .iter()
            .any(|note| note.contains("cycle member:"))
    );
    let primary = cycle.primary.as_ref().expect("cycle primary label");
    assert!(primary.message.contains("include leg 2/2"));
    assert!(primary.message.contains("b.raql"));
    assert!(primary.message.contains("a.raql"));
    assert!(
        cycle
            .secondary
            .iter()
            .any(|label| label.message.contains("include leg 1/2")
                && label.message.contains("a.raql")
                && label.message.contains("b.raql"))
    );
    assert!(diagnostics.iter().any(|d| {
        d.kind == DiagnosticKind::IncludeCycle
            && d.include_stack
                .last()
                .is_some_and(|p| p.ends_with("a.raql"))
    }));
    assert!(diagnostics.iter().any(|d| {
        d.notes
            .iter()
            .any(|note| note.contains("a.raql") || note.contains("b.raql"))
    }));
}

#[test]
fn parse_diagnostic_stack_includes_current_file() {
    let root = temp_dir("include_parse_stack");
    let entry = root.join("main.raql");
    let middle = root.join("middle.raql");
    let leaf = root.join("leaf.raql");

    fs::write(entry.as_std_path(), ".include \"middle.raql\".\n").expect("write entry");
    fs::write(middle.as_std_path(), ".include \"leaf.raql\".\n").expect("write middle");
    fs::write(leaf.as_std_path(), ".decl broken(X: int input.\n").expect("write leaf");

    let loader = IncludeLoader::new(vec![]);
    let diagnostics = loader
        .load_program(entry.as_path())
        .expect_err("loader should fail for parse error");

    let parse_diag = diagnostics
        .iter()
        .find(|d| {
            d.kind == DiagnosticKind::Parse
                && d.message
                    .contains("expected `)` after declaration arguments")
        })
        .expect("parse error diagnostic for leaf");
    assert!(
        parse_diag
            .include_stack
            .last()
            .is_some_and(|p| p.ends_with("leaf.raql"))
    );
    assert!(
        parse_diag
            .secondary
            .iter()
            .any(|label| label.message.contains("include leg 1/2")
                && label.message.contains("main.raql")
                && label.message.contains("middle.raql"))
    );
    assert!(
        parse_diag
            .secondary
            .iter()
            .any(|label| label.message.contains("include leg 2/2")
                && label.message.contains("middle.raql")
                && label.message.contains("leaf.raql"))
    );
    assert!(parse_diag.notes.iter().any(|note| {
        note.contains("this file was pulled in by")
            && note.contains("leaf.raql")
            && note.contains("middle.raql")
    }));
}
