//! Key handling for the picker.

use super::*;

impl<'a> PickApp<'a> {
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickOutcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(PickOutcome::Aborted);
        }

        match self.stage {
            Stage::SelectRecipe => self.handle_stage1_key(key),
            Stage::FillPlaceholder(idx) => self.handle_stage2_key(idx, key),
        }
    }

    pub(super) fn handle_stage1_key(&mut self, key: KeyEvent) -> Option<PickOutcome> {
        let len = self.filtered_recipes.len();
        if let Some(sel) = move_selection(self.recipe_selected, len, &key, self.page_rows) {
            self.recipe_selected = sel;
            return None;
        }

        match key.code {
            KeyCode::Esc => return Some(PickOutcome::Aborted),
            KeyCode::Enter => {
                if let Some((_, recipe)) = self.filtered_recipes.get(self.recipe_selected).cloned()
                {
                    self.enter_recipe(recipe);
                    if let Some(done) = self.is_immediately_complete() {
                        return Some(PickOutcome::Selected(done));
                    }
                }
            }
            _ => {
                if edit_query(&mut self.recipe_query, &key) {
                    self.recipe_selected = 0;
                    self.refresh_recipes();
                }
            }
        }
        None
    }

    pub(super) fn handle_stage2_key(&mut self, idx: usize, key: KeyEvent) -> Option<PickOutcome> {
        let Some(ph) = self.placeholders.get(idx).cloned() else {
            if let Some(recipe) = &self.active_recipe {
                return Some(PickOutcome::Selected(substitute_recipe_template(
                    &recipe.template,
                    &self.filled_values,
                )));
            }
            return Some(PickOutcome::Aborted);
        };

        if let Some(delta) = preview_scroll_delta(&key, self.page_rows) {
            self.scroll_preview(delta);
            return None;
        }
        let len = self.placeholder_candidates.len();
        if let Some(sel) = move_selection(self.placeholder_selected, len, &key, self.page_rows) {
            if sel != self.placeholder_selected {
                self.placeholder_selected = sel;
                self.mark_preview_dirty();
            }
            return None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if idx > 0 {
                    let prev_name = self.placeholders[idx - 1].name.clone();
                    self.filled_values.remove(&prev_name);
                    self.load_placeholder(idx - 1);
                } else if self.started_at_stage2 {
                    return Some(PickOutcome::Aborted);
                } else {
                    self.stage = Stage::SelectRecipe;
                    self.active_recipe = None;
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.cache.force_expire_all();
                self.preview_last_key = None;
                self.refresh_placeholder_candidates();
                self.mark_preview_dirty();
            }
            KeyCode::Char(' ') if ph.variadic && !self.placeholder_candidates.is_empty() => {
                if let Some(cand) = self.placeholder_candidates.get(self.placeholder_selected) {
                    let val = cand.insert_value.clone();
                    if let Some(pos) = self.multi_selected.iter().position(|x| *x == val) {
                        self.multi_selected.remove(pos);
                    } else {
                        self.multi_selected.push(val);
                    }
                }
            }
            KeyCode::Enter => {
                let chosen: Vec<String> = if ph.variadic && !self.multi_selected.is_empty() {
                    self.multi_selected.clone()
                } else if let Some(cand) = self.placeholder_candidates.get(self.placeholder_selected)
                {
                    vec![cand.insert_value.clone()]
                } else if ph.variadic {
                    self.placeholder_query
                        .split_whitespace()
                        .map(String::from)
                        .collect()
                } else {
                    vec![self.placeholder_query.clone()]
                };

                self.filled_values.insert(ph.name.clone(), chosen);
                if idx + 1 < self.placeholders.len() {
                    self.load_placeholder(idx + 1);
                } else if let Some(recipe) = &self.active_recipe {
                    let cmd = substitute_recipe_template(&recipe.template, &self.filled_values);
                    return Some(PickOutcome::Selected(cmd));
                }
            }
            _ => {
                if edit_query(&mut self.placeholder_query, &key) {
                    self.placeholder_selected = 0;
                    self.refresh_placeholder_candidates();
                    self.mark_preview_dirty();
                }
            }
        }
        None
    }

    pub fn is_immediately_complete(&self) -> Option<String> {
        if let Stage::FillPlaceholder(idx) = self.stage {
            if idx >= self.placeholders.len() {
                if let Some(recipe) = &self.active_recipe {
                    return Some(substitute_recipe_template(
                        &recipe.template,
                        &self.filled_values,
                    ));
                }
            }
        }
        None
    }
}

/// Preview scroll keys: Shift/Alt-Up/Down by 3 lines, Shift-PgUp/PgDn by about half a page.
pub(super) fn preview_scroll_delta(key: &KeyEvent, page: usize) -> Option<isize> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let half = (page / 2).max(3) as isize;
    match key.code {
        KeyCode::Up if shift || alt => Some(-3),
        KeyCode::Down if shift || alt => Some(3),
        KeyCode::PageUp if shift => Some(-half),
        KeyCode::PageDown if shift => Some(half),
        _ => None,
    }
}

/// Maps navigation keys to the new selection: single steps wrap around, pages clamp.
pub(super) fn move_selection(sel: usize, len: usize, key: &KeyEvent, page: usize) -> Option<usize> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let page = page.max(1) as isize;
    let next = match key.code {
        KeyCode::Up | KeyCode::BackTab => wrap_step(sel, len, -1),
        KeyCode::Down | KeyCode::Tab => wrap_step(sel, len, 1),
        KeyCode::Char('l') if alt => wrap_step(sel, len, 1),
        KeyCode::Char('p') if ctrl => wrap_step(sel, len, -1),
        KeyCode::Char('n') if ctrl => wrap_step(sel, len, 1),
        KeyCode::PageUp => clamp_step(sel, len, -page),
        KeyCode::PageDown => clamp_step(sel, len, page),
        _ => return None,
    };
    Some(next)
}

pub(super) fn wrap_step(sel: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (sel as isize + delta).rem_euclid(len as isize) as usize
}

pub(super) fn clamp_step(sel: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (sel as isize + delta).clamp(0, len as isize - 1) as usize
}

/// Applies line-editing keys to `query`; returns whether it changed.
pub(super) fn edit_query(query: &mut String, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Backspace if ctrl || alt => delete_word(query),
        KeyCode::Backspace => query.pop().is_some(),
        KeyCode::Char('w') if ctrl => delete_word(query),
        KeyCode::Char('u') if ctrl => {
            let changed = !query.is_empty();
            query.clear();
            changed
        }
        KeyCode::Char(ch) if !ctrl && !alt => {
            query.push(ch);
            true
        }
        _ => false,
    }
}

pub(super) fn delete_word(query: &mut String) -> bool {
    let before = query.len();
    let trimmed = query.trim_end().len();
    query.truncate(trimmed);
    let cut = query
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    query.truncate(cut);
    query.len() != before
}
