use crate::spec::ast::Spec;
use crate::spec::parse_and_validate;

pub const BUNDLED_SPECS: &[(&str, &str)] = &[
    ("bundled:fs.ezlo", include_str!("../specs/fs.ezlo")),
    ("bundled:git.ezlo", include_str!("../specs/git.ezlo")),
];

pub fn load_bundled_specs() -> Vec<(String, Spec)> {
    let mut out = Vec::with_capacity(BUNDLED_SPECS.len());
    for &(name, src) in BUNDLED_SPECS {
        let (spec, _diags) = parse_and_validate(src);
        out.push((name.to_string(), spec));
    }
    out
}
