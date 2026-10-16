use super::TypeRow;
use crate::spec::ast::{Clause, CompareOp};
use regex::Regex;
use std::collections::HashMap;
use std::slice;
use std::sync::{Mutex, OnceLock, PoisonError};

pub fn matches_clauses(row: &[String], cols: &[String], clauses: &[Clause]) -> bool {
    clauses.iter().all(|clause| {
        let Some(col_idx) = cols.iter().position(|c| c == &clause.col) else {
            return false;
        };
        let cell = row.get(col_idx).map(String::as_str).unwrap_or("");
        match clause.op {
            CompareOp::Eq => clause.values.first().map(String::as_str) == Some(cell),
            CompareOp::Ne => clause.values.first().map(String::as_str) != Some(cell),
            CompareOp::In => clause.values.iter().any(|v| v == cell),
            CompareOp::Match => clause
                .values
                .first()
                .is_some_and(|pat| regex_is_match(pat, cell)),
        }
    })
}

/// Matches `cell` against `pat`, compiling each distinct pattern once per process.
/// Invalid patterns are cached as `None` and never match.
fn regex_is_match(pat: &str, cell: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let re = {
        let mut cache = CACHE
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = cache.get(pat) {
            entry.clone()
        } else {
            let compiled = Regex::new(pat).ok();
            cache.insert(pat.to_string(), compiled.clone());
            compiled
        }
    };
    re.is_some_and(|re| re.is_match(cell))
}

/// Applies `where`/`rank` clauses to builtin path rows, whose only column is `value`.
pub(super) fn filter_and_rank_builtin_rows(
    mut rows: Vec<TypeRow>,
    where_chain: &[&[Clause]],
    rank: Option<&[Clause]>,
) -> Vec<TypeRow> {
    let cols = ["value".to_string()];
    rows.retain(|r| {
        let cell = slice::from_ref(&r.insert_value);
        where_chain
            .iter()
            .all(|clauses| matches_clauses(cell, &cols, clauses))
    });
    if let Some(rclauses) = rank {
        for r in &mut rows {
            if !matches_clauses(slice::from_ref(&r.insert_value), &cols, rclauses) {
                r.rank_deemphasized = true;
            }
        }
    }
    rows
}
