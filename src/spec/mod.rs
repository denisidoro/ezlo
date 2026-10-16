pub mod ast;
pub mod lexer;
pub mod parser;
pub mod preview;
pub mod span;
pub mod validate;

use crate::spec::ast::{CmdDecl, OptDecl, RecipeDecl, Spec, TypeDecl};
use crate::spec::parser::parse_spec_raw;
use crate::spec::span::{Diagnostic, RuleCode, Severity};
use crate::spec::validate::validate_spec;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// File extension (without the dot) of spec files: user, generated and bundled specs are `*.ezlo`.
pub const SPEC_EXT: &str = "ezlo";

pub fn parse_and_validate(source: &str) -> (Spec, Vec<Diagnostic>) {
    let (spec, mut diags) = parse_spec_raw(source);
    validate_spec(&spec, &mut diags);
    (spec, diags)
}

pub fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SpecIndex {
    pub types: Vec<TypeDecl>,
    pub cmds: Vec<CmdDecl>,
    pub recipes: Vec<RecipeDecl>,
    /// Where each merged `cmd` path (space-joined) was declared, highest precedence first.
    /// Required on disk on purpose: indexes cached before it existed fail to load and are rebuilt.
    pub origins: HashMap<String, Vec<CmdOrigin>>,
    /// Top-level `alias` declarations: command word -> expansion words, highest precedence first.
    /// Required on disk for the same reason as `origins`.
    pub aliases: HashMap<String, Vec<String>>,
    /// Root-word lookup over `cmds`, built on first use (not cached on disk). `cmds` must not be
    /// mutated once lookups started.
    #[serde(skip)]
    roots: OnceLock<RootLookup>,
}

/// Positions in [`SpecIndex::cmds`] grouped by root word, plus root-command aliases.
#[derive(Debug, Clone, Default)]
struct RootLookup {
    /// Root word -> positions (ascending) of every `cmd` whose path starts with it.
    cmds: HashMap<String, Vec<u32>>,
    /// Alias of a root command -> canonical root word (first declaring command wins).
    aliases: HashMap<String, String>,
}

impl RootLookup {
    fn build(cmds: &[CmdDecl]) -> Self {
        let mut lookup = Self::default();
        for (pos, cmd) in (0u32..).zip(cmds) {
            let Some(root) = cmd.path.first() else { continue };
            lookup.cmds.entry(root.clone()).or_default().push(pos);
            if cmd.path.len() == 1 {
                for alias in &cmd.aliases {
                    lookup.aliases.entry(alias.clone()).or_insert_with(|| root.clone());
                }
            }
        }
        lookup
    }
}

/// Spec tier a declaration comes from (`ezlo explain` provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecTier {
    User,
    Bundled,
    Generated,
}

impl SpecTier {
    /// Tiers in precedence order, as passed to [`SpecIndex::merge_tiers`] by the index loader.
    pub const ORDER: [SpecTier; 3] = [SpecTier::User, SpecTier::Bundled, SpecTier::Generated];

    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user spec",
            Self::Bundled => "bundled spec",
            Self::Generated => "generated spec",
        }
    }
}

/// Specs of one precedence tier with their provenance label.
type LabeledTier<'s> = (Option<SpecTier>, &'s [(String, Spec)]);

/// One spec file contributing to a merged `cmd` path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CmdOrigin {
    /// `None` when the specs weren't merged as user > bundled > generated tiers.
    pub tier: Option<SpecTier>,
    pub source: String,
}

impl SpecIndex {
    pub fn from_single(spec: Spec) -> Self {
        Self {
            types: spec.types,
            cmds: spec.cmds,
            recipes: spec.recipes,
            origins: HashMap::new(),
            aliases: spec.aliases.into_iter().map(|a| (a.name, a.expansion)).collect(),
            roots: OnceLock::new(),
        }
    }

    fn roots(&self) -> &RootLookup {
        self.roots.get_or_init(|| RootLookup::build(&self.cmds))
    }

    /// Commands whose path starts with `prefix`, in declaration order (every command when empty).
    /// Only the commands sharing `prefix`'s root word are visited.
    pub fn cmds_with_prefix<'s, 'p>(
        &'s self,
        prefix: &'p [String],
    ) -> impl Iterator<Item = &'s CmdDecl> + use<'s, 'p> {
        let (positions, all): (&[u32], &[CmdDecl]) = match prefix.first() {
            Some(root) => (self.roots().cmds.get(root).map_or(&[][..], Vec::as_slice), &[]),
            None => (&[], &self.cmds),
        };
        positions
            .iter()
            .filter_map(|&pos| self.cmds.get(pos as usize))
            .chain(all)
            .filter(move |cmd| cmd.path.starts_with(prefix))
    }

    /// The command declared exactly at `path`, if any.
    pub fn find_cmd(&self, path: &[String]) -> Option<&CmdDecl> {
        self.cmds_with_prefix(path).find(|cmd| cmd.path == path)
    }

    /// Whether some command path starts with `word`.
    pub fn has_root(&self, word: &str) -> bool {
        self.roots().cmds.contains_key(word)
    }

    /// Canonical root word of the root command declaring `alias`, if any.
    pub fn root_alias_target(&self, alias: &str) -> Option<&str> {
        self.roots().aliases.get(alias).map(String::as_str)
    }

    /// Merge specs ordered from highest precedence to lowest precedence (PROMPT.md §3), each file being its own tier.
    /// See [`SpecIndex::merge_tiers`]. Origins are recorded without a [`SpecTier`].
    pub fn merge_ordered(specs: &[(String, Spec)], diags: &mut Vec<(String, Diagnostic)>) -> Self {
        let tiers: Vec<LabeledTier<'_>> = specs.chunks(1).map(|c| (None, c)).collect();
        Self::merge_labeled(&tiers, diags)
    }

    /// Spec files the command declared exactly at `path` comes from, highest precedence first.
    pub fn origins_of(&self, path: &[String]) -> &[CmdOrigin] {
        self.origins.get(&path.join(" ")).map_or(&[], Vec::as_slice)
    }

    /// Merge tiers of specs ordered from highest precedence to lowest precedence (e.g. user > bundled > generated):
    /// - Grammar (`cmds`) is deep-overlaid across tiers: a higher-tier `cmd` with the same path as a lower-tier one
    ///   is merged on top of it (see [`overlay_cmd`]), and paths declared by only one tier are kept as-is, so a
    ///   user `git.ezlo` can add or override a few flags/subcommands while preserving the rest of the base grammar.
    /// - Files within the same tier extend each other's roots (e.g. `dot/ssh.ezlo` + `dot/docker.ezlo`);
    ///   a cmd path declared by several files of a tier is taken from the first one, with a warning in `diags`.
    /// - `recipes` from all files are merged; a recipe with the same scope and template as a higher-tier one is dropped.
    /// - Duplicate type names across files => higher precedence wins with a warning in `diags`.
    ///
    /// Tiers are labeled by position with [`SpecTier::ORDER`] (user, bundled, generated) for provenance.
    pub fn merge_tiers(tiers: &[&[(String, Spec)]], diags: &mut Vec<(String, Diagnostic)>) -> Self {
        let labeled: Vec<LabeledTier<'_>> = tiers
            .iter()
            .enumerate()
            .map(|(i, specs)| (SpecTier::ORDER.get(i).copied(), *specs))
            .collect();
        Self::merge_labeled(&labeled, diags)
    }

    /// [`SpecIndex::merge_tiers`] with an explicit provenance label per tier.
    fn merge_labeled(tiers: &[LabeledTier<'_>], diags: &mut Vec<(String, Diagnostic)>) -> Self {
        let mut index = Self::default();
        let mut seen_types: HashMap<String, String> = HashMap::new();
        // Position of each merged path in `index.cmds`.
        let mut merged_paths: HashMap<Vec<String>, usize> = HashMap::new();
        let mut seen_recipes: HashSet<(Vec<String>, String)> = HashSet::new();

        for &(tier, specs) in tiers.iter() {
            let mut tier_paths: HashMap<Vec<String>, String> = HashMap::new();
            let mut tier_recipes: HashSet<(Vec<String>, String)> = HashSet::new();
            let mut tier_aliases: HashMap<&str, &str> = HashMap::new();
            for (source_name, spec) in specs.iter() {
                index.merge_types(source_name, spec, &mut seen_types, diags);
                index.merge_aliases(source_name, spec, &mut tier_aliases, diags);

                for cmd in &spec.cmds {
                    if cmd.path.is_empty() {
                        continue;
                    }
                    if let Some(prev_file) = tier_paths.get(&cmd.path) {
                        diags.push((
                            source_name.clone(),
                            Diagnostic::warning(
                                RuleCode::E04Duplicate,
                                cmd.span,
                                format!(
                                    "duplicate cmd '{}' across files; shadowed by '{prev_file}'",
                                    cmd.path.join(" ")
                                ),
                            ),
                        ));
                        continue;
                    }
                    tier_paths.insert(cmd.path.clone(), source_name.clone());
                    index.origins.entry(cmd.path.join(" ")).or_default().push(CmdOrigin {
                        tier,
                        source: source_name.clone(),
                    });
                    match merged_paths.get(&cmd.path) {
                        Some(&pos) => {
                            let overlay = std::mem::replace(&mut index.cmds[pos], cmd.clone());
                            overlay_cmd(&mut index.cmds[pos], overlay);
                        }
                        None => {
                            merged_paths.insert(cmd.path.clone(), index.cmds.len());
                            index.cmds.push(cmd.clone());
                        }
                    }
                }

                for rec in &spec.recipes {
                    let key = (rec.scope.clone(), rec.template.clone());
                    if seen_recipes.contains(&key) {
                        continue;
                    }
                    tier_recipes.insert(key);
                    index.recipes.push(rec.clone());
                }
            }
            seen_recipes.extend(tier_recipes);
        }

        index
    }

    /// Adds `spec`'s top-level aliases unless a higher tier already declared them; within a tier,
    /// the first file declaring a name wins (with a warning).
    fn merge_aliases<'s>(
        &mut self,
        source_name: &'s str,
        spec: &'s Spec,
        tier_aliases: &mut HashMap<&'s str, &'s str>,
        diags: &mut Vec<(String, Diagnostic)>,
    ) {
        for alias in &spec.aliases {
            if let Some(prev_file) = tier_aliases.get(alias.name.as_str()) {
                diags.push((
                    source_name.to_owned(),
                    Diagnostic::warning(
                        RuleCode::E04Duplicate,
                        alias.span,
                        format!("duplicate alias '{}' across files; shadowed by '{prev_file}'", alias.name),
                    ),
                ));
                continue;
            }
            tier_aliases.insert(&alias.name, source_name);
            self.aliases.entry(alias.name.clone()).or_insert_with(|| alias.expansion.clone());
        }
    }

    fn merge_types(
        &mut self,
        source_name: &str,
        spec: &Spec,
        seen_types: &mut HashMap<String, String>,
        diags: &mut Vec<(String, Diagnostic)>,
    ) {
        for td in &spec.types {
            let tname = td.name();
            if let Some(prev_file) = seen_types.get(tname) {
                diags.push((
                    source_name.to_string(),
                    Diagnostic::warning(
                        RuleCode::E04Duplicate,
                        td.span(),
                        format!("duplicate type '{tname}' across files; shadowed by higher-precedence '{prev_file}'"),
                    ),
                ));
            } else {
                seen_types.insert(tname.to_string(), source_name.to_string());
                self.types.push(td.clone());
            }
        }
    }

    pub fn find_type(&self, name: &str) -> Option<&TypeDecl> {
        self.types.iter().find(|t| t.name() == name)
    }

    pub fn type_map(&self) -> HashMap<&str, &TypeDecl> {
        self.types.iter().map(|t| (t.name(), t)).collect()
    }
}

/// Deep-merges `overlay` (higher precedence) on top of `base` (same command path), in place:
/// - `desc` comes from the overlay when it declares one; aliases are unioned.
/// - An overlay option replaces every base option sharing one of its flags (or its canonical name);
///   other base options are kept and new overlay options are appended.
/// - An overlay positional replaces the base positional with the same name in place; new ones are appended.
pub fn overlay_cmd(base: &mut CmdDecl, overlay: CmdDecl) {
    if overlay.desc.is_some() {
        base.desc = overlay.desc;
    }
    if overlay.preview.is_some() {
        base.preview = overlay.preview;
        base.preview_span = overlay.preview_span;
    }
    for alias in overlay.aliases {
        if !base.aliases.contains(&alias) {
            base.aliases.push(alias);
        }
    }

    let mut opts = Vec::with_capacity(base.opts.len() + overlay.opts.len());
    let mut used = vec![false; overlay.opts.len()];
    for opt in std::mem::take(&mut base.opts) {
        let overrides = |o: &OptDecl| o.name == opt.name || o.flags.iter().any(|f| opt.flags.contains(f));
        match overlay.opts.iter().position(overrides) {
            Some(i) if !used[i] => {
                used[i] = true;
                opts.push(overlay.opts[i].clone());
            }
            Some(_) => {}
            None => opts.push(opt),
        }
    }
    opts.extend(overlay.opts.into_iter().zip(used).filter(|(_, u)| !u).map(|(o, _)| o));
    base.opts = opts;

    for arg in overlay.args {
        match base.args.iter_mut().find(|a| a.name == arg.name) {
            Some(slot) => *slot = arg,
            None => base.args.push(arg),
        }
    }
    base.recipes.extend(overlay.recipes);
}
