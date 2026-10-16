use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ezlo::engine::complete_line;
use ezlo::engine::generator::{FakeGeneratorRunner, GeneratorCache};
use ezlo::engine::rank::EmptyUsageLookup;
use ezlo::engine::CompletionRequest;
use ezlo::spec::{parse_and_validate, SpecIndex};

fn build_2000_option_spec() -> SpecIndex {
    let mut src = String::from("cmd mega \"Mega CLI with 2000 options\"\n");
    for i in 0..2000 {
        let _ = writeln!(
            src,
            "  opt --option-flag-{i:04} <val> \"Option number {i}\""
        );
    }
    let (spec, diags) = parse_and_validate(&src);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");

    SpecIndex::from_single(spec)
}

fn bench_completion_2000_opts(c: &mut Criterion) {
    let index = build_2000_option_spec();
    let runner = FakeGeneratorRunner::default();
    let cache = GeneratorCache::new();
    let usage = EmptyUsageLookup;
    let cwd = Path::new("/tmp");
    let env = HashMap::new();
    let aliases = HashMap::new();
    let wrappers: Vec<String> = Vec::new();
    let line = "mega --option-flag-15";

    let start = Instant::now();
    let req = CompletionRequest {
        line,
        cursor: line.len(),
        cwd,
        env: &env,
        aliases: &aliases,
        wrappers: &wrappers,
        index: &index,
        runner: &runner,
        cache: &cache,
        usage: &usage,
        timeout: Duration::from_secs(1),
        now_ts: 1_700_000_000,
        allow_stale: false,
    };
    let resp = complete_line(&req);
    let elapsed = start.elapsed();
    assert!(!resp.candidates.is_empty());
    assert!(
        elapsed.as_millis() < 5,
        "2000-option completion took {:?}, exceeding 5ms budget",
        elapsed
    );

    c.bench_function("complete_2000_options", |b| {
        b.iter(|| {
            let req = CompletionRequest {
                line: black_box(line),
                cursor: black_box(line.len()),
                cwd,
                env: &env,
                aliases: &aliases,
                wrappers: &wrappers,
                index: &index,
                runner: &runner,
                cache: &cache,
                usage: &usage,
                timeout: Duration::from_secs(1),
                now_ts: 1_700_000_000,
                allow_stale: false,
            };
            black_box(complete_line(&req))
        })
    });
}

criterion_group!(benches, bench_completion_2000_opts);
criterion_main!(benches);
