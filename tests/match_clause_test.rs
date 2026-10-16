use ezlo::engine::types::clause::matches_clauses;
use ezlo::spec::ast::{Clause, CompareOp};

fn match_clause(col: &str, pat: &str) -> Clause {
    Clause {
        col: col.into(),
        op: CompareOp::Match,
        values: vec![pat.into()],
        span: Default::default(),
    }
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| (*c).to_string()).collect()
}

#[test]
fn match_clause_filters_rows_by_regex() {
    let cols = row(&["name", "status"]);
    let clauses = [match_clause("status", "^Run")];
    assert!(matches_clauses(&row(&["a", "Running"]), &cols, &clauses));
    assert!(!matches_clauses(&row(&["b", "Pending"]), &cols, &clauses));
    // Second evaluation of the same pattern goes through the compiled-regex cache.
    assert!(matches_clauses(&row(&["c", "Running"]), &cols, &clauses));
}

#[test]
fn match_clause_with_invalid_regex_never_matches() {
    let cols = row(&["name"]);
    let clauses = [match_clause("name", "(unclosed")];
    assert!(!matches_clauses(&row(&["(unclosed"]), &cols, &clauses));
    assert!(!matches_clauses(&row(&["anything"]), &cols, &clauses));
}

#[test]
fn match_clause_without_pattern_never_matches() {
    let cols = row(&["name"]);
    let clause = Clause { values: vec![], ..match_clause("name", "") };
    assert!(!matches_clauses(&row(&["x"]), &cols, &[clause]));
}
