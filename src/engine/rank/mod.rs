pub mod layout;
mod matching;

pub use matching::{classify_match, match_query, MatchBand};

use crate::engine::candidate::Candidate;

pub const HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;

#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub accepts: u64,
    pub executions: u64,
    pub last_ts: i64,
    pub used_in_cwd: bool,
    /// Executions of lines using this token that exited with a failure status.
    pub failures: u64,
}

/// Failures needed before a token counts as chronically failing.
pub const CHRONIC_FAILURES: u64 = 2;
/// Score subtracted from a token that (almost) always fails.
pub const MAX_FAILURE_PENALTY: f64 = 4.0;

impl UsageRecord {
    pub fn compute_score(&self, now_ts: i64) -> f64 {
        let base = (3 * self.accepts + self.executions) as f64;
        if base <= 0.0 {
            return if self.used_in_cwd { 0.5 } else { 0.0 };
        }
        let age_secs = (now_ts - self.last_ts).max(0) as f64;
        let decay = 0.5_f64.powf(age_secs / HALF_LIFE_SECS);
        let cwd_bonus = if self.used_in_cwd { 1.5 } else { 0.0 };
        base * decay + cwd_bonus
    }

    /// Ranking penalty of a chronically failing token: proportional to its failure ratio, and only
    /// once it failed at least `CHRONIC_FAILURES` times (a single failure is just noise).
    pub fn failure_penalty(&self) -> f64 {
        if self.failures < CHRONIC_FAILURES {
            return 0.0;
        }
        let successes = self.accepts + self.executions;
        MAX_FAILURE_PENALTY * self.failures as f64 / (self.failures + successes) as f64
    }
}

pub trait UsageLookup {
    fn lookup_usage(&self, ctx: &str, label: &str, cwd: &str) -> UsageRecord;

    fn learned_values(&self, _ctx: &str) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Debug, Default, Clone)]
pub struct EmptyUsageLookup;

impl UsageLookup for EmptyUsageLookup {
    fn lookup_usage(&self, _ctx: &str, _label: &str, _cwd: &str) -> UsageRecord {
        UsageRecord::default()
    }
}

pub fn filter_and_rank_candidates(
    raw: Vec<Candidate>,
    typed_prefix: &str,
    usage_ctx: &str,
    cwd: &str,
    usage: &dyn UsageLookup,
    now_ts: i64,
) -> Vec<Candidate> {
    struct ScoredItem {
        cand: Candidate,
        band: MatchBand,
        matched_label: bool,
        source_order: usize,
    }

    let mut matched: Vec<ScoredItem> = Vec::with_capacity(raw.len());
    for (idx, mut cand) in raw.into_iter().enumerate() {
        let (band, matched_label) = if let Some(b) = classify_match(typed_prefix, &cand.label) {
            (b, true)
        } else if cand.insert_value != cand.label {
            let Some(b) = classify_match(typed_prefix, &cand.insert_value) else {
                continue;
            };
            (b, false)
        } else {
            continue;
        };
        let urec = usage.lookup_usage(usage_ctx, &cand.label, cwd);
        let mut score = urec.compute_score(now_ts).max(cand.score) - urec.failure_penalty();
        if cand.deemphasized {
            score -= 100.0;
        }
        cand.score = score;
        matched.push(ScoredItem {
            cand,
            band,
            matched_label,
            source_order: idx,
        });
    }

    matched.sort_by(|a, b| {
        a.cand
            .deemphasized
            .cmp(&b.cand.deemphasized)
            .then_with(|| a.band.cmp(&b.band))
            .then_with(|| a.cand.priority_tier.cmp(&b.cand.priority_tier))
            .then_with(|| b.cand.score.total_cmp(&a.cand.score))
            .then_with(|| a.source_order.cmp(&b.source_order))
    });

    matched
        .into_iter()
        .enumerate()
        .map(|(out_idx, mut item)| {
            if out_idx < 50 && item.matched_label && !typed_prefix.is_empty()
                && let Some((_, indices)) = match_query(typed_prefix, &item.cand.label) {
                    item.cand.match_indices = indices;
                }
            item.cand
        })
        .collect()
}
