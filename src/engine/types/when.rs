use crate::spec::ast::WhenClause;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenStatus {
    True,
    Pending(String),
    Contradicted,
}

pub fn evaluate_when_status(
    clause: &WhenClause,
    present_flags: &HashMap<String, Option<String>>,
) -> WhenStatus {
    let mut missing = Vec::new();
    for term in &clause.cond {
        let present = present_flags.get(&term.flag);
        if term.negated {
            if present.is_some() {
                return WhenStatus::Contradicted;
            }
        } else if let Some(expected) = &term.value {
            match present {
                Some(Some(actual)) if actual == expected => {}
                Some(_) => return WhenStatus::Contradicted,
                None => missing.push(format!("{}={expected}", term.flag)),
            }
        } else if present.is_none() {
            missing.push(term.flag.clone());
        }
    }

    if missing.is_empty() {
        WhenStatus::True
    } else {
        WhenStatus::Pending(format!("requires {}", missing.join(" ")))
    }
}
