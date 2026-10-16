#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
pub enum MatchBand {
    Prefix = 0,
    Fuzzy = 1,
}

pub fn classify_match(query: &str, target: &str) -> Option<MatchBand> {
    if query.is_empty() {
        return Some(MatchBand::Prefix);
    }
    if query.is_ascii() && target.is_ascii() {
        let qb = query.as_bytes();
        let tb = target.as_bytes();
        if tb.len() < qb.len() {
            return None;
        }
        if tb[..qb.len()].eq_ignore_ascii_case(qb) {
            return Some(MatchBand::Prefix);
        }
        let mut qi = 0;
        for &b in tb {
            if b.eq_ignore_ascii_case(&qb[qi]) {
                qi += 1;
                if qi == qb.len() {
                    return Some(MatchBand::Fuzzy);
                }
            }
        }
        return None;
    }

    let mut q_iter = query.chars().map(|c| c.to_ascii_lowercase());
    let mut t_iter = target.chars().map(|c| c.to_ascii_lowercase());
    let mut is_prefix = true;
    let mut cur_q = q_iter.next();
    while let Some(qc) = cur_q {
        match t_iter.next() {
            Some(tc) if tc == qc => {
                cur_q = q_iter.next();
            }
            Some(_) => {
                is_prefix = false;
                break;
            }
            None => return None,
        }
    }
    if is_prefix {
        return Some(MatchBand::Prefix);
    }
    while let Some(qc) = cur_q {
        if !t_iter.by_ref().any(|tc| tc == qc) {
            return None;
        }
        cur_q = q_iter.next();
    }
    Some(MatchBand::Fuzzy)
}

pub fn match_query(query: &str, target: &str) -> Option<(MatchBand, Vec<usize>)> {
    if query.is_empty() {
        return Some((MatchBand::Prefix, Vec::new()));
    }
    if query.is_ascii() && target.is_ascii() {
        let qb = query.as_bytes();
        let tb = target.as_bytes();
        if tb.len() < qb.len() {
            return None;
        }
        if tb[..qb.len()].eq_ignore_ascii_case(qb) {
            return Some((MatchBand::Prefix, (0..qb.len()).collect()));
        }
        let mut indices = Vec::with_capacity(qb.len());
        for (ti, b) in tb.iter().enumerate() {
            let Some(q) = qb.get(indices.len()) else { break };
            if b.eq_ignore_ascii_case(q) {
                indices.push(ti);
            }
        }
        return (indices.len() == qb.len()).then_some((MatchBand::Fuzzy, indices));
    }

    let q_chars: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t_chars: Vec<char> = target.chars().map(|c| c.to_ascii_lowercase()).collect();

    if t_chars.len() >= q_chars.len() && t_chars[..q_chars.len()] == q_chars[..] {
        let indices = (0..q_chars.len()).collect();
        return Some((MatchBand::Prefix, indices));
    }

    let mut indices = Vec::with_capacity(q_chars.len());
    for (ti, tc) in t_chars.iter().enumerate() {
        if q_chars.get(indices.len()) == Some(tc) {
            indices.push(ti);
        }
    }
    (indices.len() == q_chars.len()).then_some((MatchBand::Fuzzy, indices))
}
