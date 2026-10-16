//! Picker state transitions: recipe/placeholder stages, candidates and previews.

use super::*;

impl<'a> PickApp<'a> {
    pub fn new(
        index: &'a SpecIndex,
        runner: &'a dyn GeneratorRunner,
        cache: &'a GeneratorCache,
        opts: &'a PickOptions,
    ) -> Self {
        let mut scope_prefix = Vec::new();
        let mut initial_query = opts.query.clone().unwrap_or_default();

        if let Some(line) = &opts.line {
            let cursor = opts.cursor.unwrap_or(line.len()).min(line.len());
            let aliases = Aliases::new(index, &opts.aliases);
            let (parsed, walk) = walk_nested_command_line(
                index,
                parse_command_line(line, cursor, aliases, &opts.wrappers),
                aliases,
                &opts.wrappers,
                |_, _| {},
            );
            if !walk.cmd_path.is_empty() {
                scope_prefix = walk.cmd_path;
            }
            if opts.query.is_none()
                && !parsed.typed_prefix.is_empty()
                && parsed.cursor_word_idx > Some(parsed.cmd_word_idx)
            {
                initial_query = parsed.typed_prefix;
            }
        }

        let mut app = Self {
            index,
            runner,
            cache,
            cwd: &opts.cwd,
            env: &opts.env,
            scope_prefix,
            recipe_usage: HashMap::new(),
            stage: Stage::SelectRecipe,
            started_at_stage2: false,
            page_rows: opts.rows.unwrap_or(DEFAULT_PAGE_ROWS).max(1),
            list_offset: Cell::new(0),
            recipe_query: initial_query,
            filtered_recipes: Vec::new(),
            recipe_selected: 0,
            active_recipe: None,
            placeholders: Vec::new(),
            filled_values: HashMap::new(),
            placeholder_query: String::new(),
            placeholder_candidates: Vec::new(),
            placeholder_selected: 0,
            multi_selected: Vec::new(),
            preview_text: None,
            preview_rendered: None,
            preview_dirty_since: None,
            preview_last_key: None,
            preview_seq: 0,
            preview_scroll: 0,
        };

        app.refresh_recipes();

        if let Some(target_id) = &opts.recipe_id {
            let all = search_recipes(index, "", &app.scope_prefix, &app.recipe_usage);
            if let Some((_, found)) = all.into_iter().find(|(idx, r)| {
                idx.to_string() == *target_id
                    || r.template == *target_id
                    || r.desc == *target_id
            }) {
                app.started_at_stage2 = true;
                app.enter_recipe(found.clone());
            }
        }

        app
    }

    pub fn refresh_recipes(&mut self) {
        self.filtered_recipes =
            search_recipes(self.index, &self.recipe_query, &self.scope_prefix, &self.recipe_usage)
                .into_iter()
                .map(|(idx, r)| (idx, r.clone()))
                .collect();
        if self.recipe_selected >= self.filtered_recipes.len() {
            self.recipe_selected = 0;
        }
    }

    /// Index of the highlighted row in the current stage's list.
    pub fn selected_index(&self) -> usize {
        match self.stage {
            Stage::SelectRecipe => self.recipe_selected,
            Stage::FillPlaceholder(_) => self.placeholder_selected,
        }
    }

    pub(super) fn enter_recipe(&mut self, recipe: RecipeDecl) {
        let placeholders = resolve_recipe_placeholders(&recipe, self.index);
        self.active_recipe = Some(recipe);
        self.placeholders = placeholders;
        self.filled_values.clear();
        if self.placeholders.is_empty() {
            self.stage = Stage::FillPlaceholder(0);
        } else {
            self.load_placeholder(0);
        }
    }

    pub(super) fn load_placeholder(&mut self, idx: usize) {
        self.stage = Stage::FillPlaceholder(idx);
        self.placeholder_query.clear();
        self.placeholder_selected = 0;
        self.multi_selected.clear();
        self.clear_preview();
        self.refresh_placeholder_candidates();
        self.mark_preview_dirty();
    }

    pub(super) fn refresh_placeholder_candidates(&mut self) {
        let Stage::FillPlaceholder(idx) = self.stage else {
            return;
        };
        let Some(ph) = self.placeholders.get(idx) else {
            self.placeholder_candidates.clear();
            return;
        };
        let Some(recipe) = &self.active_recipe else {
            return;
        };
        self.placeholder_candidates = candidates_for_placeholder(
            ph,
            recipe,
            &self.filled_values,
            self.index,
            self.cwd,
            self.env,
            self.runner,
            self.cache,
            Duration::from_secs(1),
            &self.placeholder_query,
        );
        if self.placeholder_selected >= self.placeholder_candidates.len() {
            self.placeholder_selected = 0;
        }
    }

    pub(super) fn mark_preview_dirty(&mut self) {
        self.preview_dirty_since = Some(Instant::now());
    }

    /// Drops the current preview and invalidates any in-flight request.
    pub(super) fn clear_preview(&mut self) {
        self.preview_text = None;
        self.preview_rendered = None;
        self.preview_last_key = None;
        self.preview_seq += 1;
        self.preview_scroll = 0;
    }

    /// Scrolls the preview by `delta` lines, within its content.
    pub(super) fn scroll_preview(&mut self, delta: isize) {
        let lines = self.preview_rendered.as_ref().map_or(0, |t| t.lines.len());
        let max = lines.saturating_sub(1) as isize;
        let next = (self.preview_scroll as isize + delta).clamp(0, max);
        self.preview_scroll = u16::try_from(next).unwrap_or(0);
    }

    /// Returns the preview command for the selected candidate once the debounce elapsed
    /// (immediately with `force`), or `None` if nothing new needs to run.
    pub fn take_preview_request(&mut self, force: bool) -> Option<PreviewRequest> {
        if self.stage == Stage::SelectRecipe {
            self.preview_text = None;
            self.preview_rendered = None;
        self.preview_rendered = None;
            return None;
        }
        let dirty_at = self.preview_dirty_since?;
        if !force && dirty_at.elapsed() < PREVIEW_DEBOUNCE {
            return None;
        }
        self.preview_dirty_since = None;

        let Some(cand) = self.placeholder_candidates.get(self.placeholder_selected) else {
            self.clear_preview();
            return None;
        };
        let Some(command) = cand.preview_cmd.clone() else {
            self.clear_preview();
            return None;
        };

        let key = (command.clone(), cand.insert_value.clone());
        if self.preview_last_key.as_ref() == Some(&key) {
            return None;
        }

        let mut params: Vec<(String, String)> = cand
            .raw_columns
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, vals) in &self.filled_values {
            if let Some(first) = vals.first() {
                params.push((k.clone(), first.clone()));
            }
        }

        self.preview_last_key = Some(key);
        self.preview_seq += 1;
        Some(PreviewRequest {
            id: self.preview_seq,
            command,
            params,
        })
    }

    /// Stores a preview result unless a newer request superseded it.
    pub fn apply_preview(&mut self, id: u64, lines: &[String]) {
        if id != self.preview_seq {
            return;
        }
        self.preview_text = (!lines.is_empty()).then(|| sanitize_preview_ansi(&lines.join("\n")));
        self.preview_rendered = self.preview_text.as_deref().map(ansi_to_text);
        self.preview_scroll = 0;
    }

    pub fn preview_text(&self) -> Option<&str> {
        self.preview_text.as_deref()
    }

    /// Synchronous variant of the preview pipeline (used without an event loop, e.g. tests).
    pub fn maybe_update_preview(&mut self, force: bool) {
        let Some(req) = self.take_preview_request(force) else {
            return;
        };
        let lines =
            self.runner
                .run_shell(&req.command, self.cwd, self.env, &req.params, PREVIEW_TIMEOUT);
        self.apply_preview(req.id, &lines);
    }
}
