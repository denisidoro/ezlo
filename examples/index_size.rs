//! Approximate heap breakdown of the daemon's spec index (the cached one it actually loads).
//!
//! `cargo run --release -p ezlo --example index_size`
//!
//! Sizes are estimates: inline struct bytes plus every heap allocation rounded like glibc malloc
//! (8-byte header, 16-byte granularity, 32-byte minimum).

use ezlo::config::Config;
use ezlo::daemon::index_loader::load_cached_index;
use ezlo::spec::ast::*;
use ezlo::spec::span::Span;
use ezlo::spec::{CmdOrigin, SpecIndex};
use std::collections::BTreeMap;
use std::mem::size_of;

/// Bytes per bucket; a bucket names what would go away (or shrink) if that data were dropped.
#[derive(Default)]
struct Tally(BTreeMap<&'static str, usize>);

impl Tally {
    fn add(&mut self, bucket: &'static str, bytes: usize) {
        *self.0.entry(bucket).or_default() += bytes;
    }
}

fn alloc(bytes: usize) -> usize {
    if bytes == 0 {
        0
    } else {
        (bytes + 8).div_ceil(16).max(2) * 16
    }
}

fn s(t: &mut Tally, bucket: &'static str, v: &str) {
    t.add(bucket, alloc(v.len()));
}

fn os(t: &mut Tally, bucket: &'static str, v: &Option<String>) {
    if let Some(v) = v {
        s(t, bucket, v);
    }
}

fn vs(t: &mut Tally, bucket: &'static str, v: &[String]) {
    t.add(bucket, alloc(std::mem::size_of_val(v)));
    v.iter().for_each(|x| s(t, bucket, x));
}

/// Heap of a `Vec<T>` buffer (elements' own heap counted by the caller).
fn buf<T>(t: &mut Tally, bucket: &'static str, v: &[T]) {
    t.add(bucket, alloc(std::mem::size_of_val(v)));
}

fn spans_in<T>(t: &mut Tally, count: usize) {
    let _ = std::marker::PhantomData::<T>;
    t.add("(of which: inline Span fields, all zero in the cache)", count * size_of::<Span>());
}

fn source(t: &mut Tally, src: &Source) {
    match src {
        Source::Opt { name, .. } | Source::Arg { name, .. } | Source::Env { name, .. } => s(t, "types: exprs", name),
        Source::Literal { value, .. } => s(t, "types: exprs", value),
    }
    spans_in::<Source>(t, 1);
}

fn clauses(t: &mut Tally, cs: &Option<Vec<Clause>>) {
    let Some(cs) = cs else { return };
    buf(t, "types: exprs", cs);
    for c in cs {
        s(t, "types: exprs", &c.col);
        vs(t, "types: exprs", &c.values);
        spans_in::<Clause>(t, 1);
    }
}

fn type_expr(t: &mut Tally, e: &TypeExpr) {
    s(t, "types: exprs", &e.type_ref.name);
    buf(t, "types: exprs", &e.type_ref.bindings);
    for b in &e.type_ref.bindings {
        s(t, "types: exprs", &b.name);
        buf(t, "types: exprs", &b.sources);
        b.sources.iter().for_each(|x| source(t, x));
        spans_in::<Binding>(t, 1);
    }
    clauses(t, &e.where_cond);
    clauses(t, &e.rank);
    spans_in::<TypeExpr>(t, 2);
}

fn composite(t: &mut Tally, c: Option<&Composite>) {
    if c.is_some() {
        t.add("opts/args: values", alloc(size_of::<Composite>()));
    }
    match c {
        Some(Composite::List { sep }) => s(t, "opts/args: values", sep),
        Some(Composite::Kv { sep, value_type }) => {
            s(t, "opts/args: values", sep);
            if let Some(e) = value_type {
                t.add("opts/args: values", alloc(size_of::<TypeExpr>()));
                type_expr(t, e);
            }
        }
        None => {}
    }
}

fn when(t: &mut Tally, ws: &[WhenClause]) {
    buf(t, "opts/args: when", ws);
    for w in ws {
        buf(t, "opts/args: when", &w.cond);
        for c in &w.cond {
            s(t, "opts/args: when", &c.flag);
            os(t, "opts/args: when", &c.value);
            spans_in::<WhenTerm>(t, 1);
        }
        type_expr(t, &w.type_expr);
        spans_in::<WhenClause>(t, 1);
    }
}

fn value_spec(t: &mut Tally, v: &ValueSpec) {
    s(t, "opts/args: values", &v.name);
    composite(t, v.composite.as_deref());
    if let Some(e) = &v.type_expr {
        t.add("types: exprs", alloc(size_of::<TypeExpr>()));
        type_expr(t, e);
    }
    vs(t, "opts/args: values", &v.suggestions);
    spans_in::<ValueSpec>(t, 1);
}

fn opt(t: &mut Tally, o: &OptDecl) {
    vs(t, "opts: flags", &o.flags);
    s(t, "opts: name", &o.name);
    if let Some(v) = &o.value {
        t.add("opts/args: values", alloc(size_of::<ValueSpec>()));
        value_spec(t, v);
    }
    os(t, "descriptions", &o.desc);
    vs(t, "opts: excludes", &o.excludes);
    when(t, &o.when);
    buf(t, "opts/args: values", &o.extra_values);
    o.extra_values.iter().for_each(|v| value_spec(t, v));
    buf(t, "spans (heap)", &o.flag_spans);
    o.flag_spans.iter().for_each(|(f, _)| s(t, "spans (heap)", f));
    buf(t, "spans (heap)", &o.exclude_spans);
    o.exclude_spans.iter().for_each(|(f, _)| s(t, "spans (heap)", f));
    spans_in::<OptDecl>(t, 1);
}

fn arg(t: &mut Tally, a: &ArgDecl) {
    s(t, "args", &a.name);
    composite(t, a.composite.as_deref());
    if let Some(e) = &a.type_expr {
        t.add("types: exprs", alloc(size_of::<TypeExpr>()));
        type_expr(t, e);
    }
    vs(t, "args", &a.suggestions);
    os(t, "descriptions", &a.desc);
    when(t, &a.when);
    spans_in::<ArgDecl>(t, 1);
}

fn recipe(t: &mut Tally, r: &RecipeDecl) {
    s(t, "descriptions", &r.desc);
    vs(t, "recipes", &r.scope);
    s(t, "recipes", &r.template);
    vs(t, "recipes", &r.placeholders);
    buf(t, "recipes", &r.args);
    for a in &r.args {
        s(t, "recipes", &a.name);
        if let Some(e) = &a.type_expr {
            t.add("types: exprs", alloc(size_of::<TypeExpr>()));
            type_expr(t, e);
        }
        vs(t, "recipes", &a.suggestions);
        os(t, "descriptions", &a.desc);
        spans_in::<RecipeArg>(t, 1);
    }
    spans_in::<RecipeDecl>(t, 2);
}

fn cmd(t: &mut Tally, c: &CmdDecl) {
    vs(t, "cmds: paths", &c.path);
    vs(t, "cmds: aliases", &c.aliases);
    os(t, "descriptions", &c.desc);
    buf(t, "opts: struct", &c.opts);
    c.opts.iter().for_each(|o| opt(t, o));
    buf(t, "args", &c.args);
    c.args.iter().for_each(|a| arg(t, a));
    buf(t, "recipes", &c.recipes);
    c.recipes.iter().for_each(|r| recipe(t, r));
    os(t, "cmds: preview", &c.preview);
    spans_in::<CmdDecl>(t, 2);
}

fn type_decl(t: &mut Tally, d: &TypeDecl) {
    match d {
        TypeDecl::Base(b) => {
            s(t, "types: decls", &b.name);
            buf(t, "types: decls", &b.params);
            for p in &b.params {
                s(t, "types: decls", &p.name);
                buf(t, "types: decls", &p.default);
                p.default.iter().for_each(|x| source(t, x));
                spans_in::<Param>(t, 1);
            }
            match &b.generator {
                Generator::Shell { command, .. } => s(t, "types: generators", command),
                Generator::Values { values, .. } => vs(t, "types: generators", values),
            }
            vs(t, "types: decls", &b.cols);
            s(t, "types: decls", &b.insert);
            vs(t, "types: decls", &b.show);
            os(t, "types: decls", &b.preview);
            buf(t, "spans (heap)", &b.col_spans);
            buf(t, "spans (heap)", &b.show_spans);
            spans_in::<BaseType>(t, 3);
        }
        TypeDecl::Refine(r) => {
            s(t, "types: decls", &r.name);
            type_expr(t, &r.refines);
            spans_in::<RefineType>(t, 1);
        }
    }
}

fn main() {
    let config = Config::load();
    let Some(index) = load_cached_index(&config) else {
        eprintln!("no fresh index cache; run `ezlo daemon --build-index` first");
        std::process::exit(1);
    };
    let mut t = Tally::default();
    measure(&mut t, &index);

    // "(of which ...)" rows are already inside their struct buffers: informational, not summed.
    let total: usize = t.0.iter().filter(|(k, _)| !k.starts_with('(')).map(|(_, v)| v).sum();
    let mut rows: Vec<_> = t.0.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    println!("cmds={} opts={} types={} recipes={}", index.cmds.len(), index.cmds.iter().map(|c| c.opts.len()).sum::<usize>(), index.types.len(), index.recipes.len());
    println!("sizeof: CmdDecl={} OptDecl={} ArgDecl={} ValueSpec={} TypeExpr={} RecipeDecl={} Span={} TypeDecl={}",
        size_of::<CmdDecl>(), size_of::<OptDecl>(), size_of::<ArgDecl>(), size_of::<ValueSpec>(), size_of::<TypeExpr>(), size_of::<RecipeDecl>(), size_of::<Span>(), size_of::<TypeDecl>());
    for (bucket, bytes) in rows {
        println!("{:>8.1} KB {:>5.1}%  {bucket}", *bytes as f64 / 1024.0, *bytes as f64 * 100.0 / total as f64);
    }
    println!("{:>8.1} KB total", total as f64 / 1024.0);
}

fn measure(t: &mut Tally, index: &SpecIndex) {
    buf(t, "cmds: struct", &index.cmds);
    index.cmds.iter().for_each(|c| cmd(t, c));
    buf(t, "types: decls", &index.types);
    index.types.iter().for_each(|d| type_decl(t, d));
    buf(t, "recipes", &index.recipes);
    index.recipes.iter().for_each(|r| recipe(t, r));
    for (k, v) in &index.origins {
        // HashMap slot (key + value) plus the joined path and source strings.
        t.add("origins", size_of::<(String, Vec<CmdOrigin>)>() + 1);
        s(t, "origins", k);
        buf(t, "origins", v);
        v.iter().for_each(|o| s(t, "origins", &o.source));
    }
    for (k, v) in &index.aliases {
        t.add("aliases", size_of::<(String, Vec<String>)>() + 1);
        s(t, "aliases", k);
        vs(t, "aliases", v);
    }
}
