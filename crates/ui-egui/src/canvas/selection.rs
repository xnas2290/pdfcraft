use super::{Arc, DocId, DocInfo, DocView, Edit, HashMap, PageText, Pos2, Range, Rect, comments, pos2};

/// A text selection on one page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Selection {
    pub(super) page: usize,
    pub(super) span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Span {
    /// Reading order, between two glyph indices.
    Run { anchor: usize, head: usize },
    /// Column select (#740): the glyphs inside a rectangle in page view space, drawn from the
    /// corner the drag started at (`rect[0..2]`) to the pointer (`rect[2..4]`).
    Column { rect: [f32; 4] },
}

impl Selection {
    pub(super) fn run(page: usize, anchor: usize, head: usize) -> Self {
        Self { page, span: Span::Run { anchor, head } }
    }

    fn range(anchor: usize, head: usize) -> Range<usize> {
        anchor.min(head)..anchor.max(head).saturating_add(1)
    }

    /// One rectangle per selected piece of a line, in view space (highlighting, markup quads).
    pub(super) fn rects(&self, text: &PageText) -> Vec<[f32; 4]> {
        match self.span {
            Span::Run { anchor, head } => text.line_rects(Self::range(anchor, head)),
            Span::Column { rect } => text.glyph_rects(&text.glyphs_in(rect)),
        }
    }

    /// The selected text, as Copy puts it on the clipboard.
    pub(super) fn text(&self, text: &PageText) -> String {
        match self.span {
            Span::Run { anchor, head } => text.text_of(Self::range(anchor, head)),
            Span::Column { rect } => text.column_text(&text.glyphs_in(rect)),
        }
    }
}

/// The glyphs `clicks` quick clicks select around glyph `a`: two the word, three the line, more
/// the page.
pub(super) fn click_unit(text: &PageText, clicks: u32, a: usize) -> Option<(usize, usize)> {
    match clicks {
        2 => text.word_at(a),
        3 => text.line_at(a),
        _ => text.glyphs.len().checked_sub(1).map(|last| (0, last)),
    }
}

impl DocView {
    /// The text selection as markup quadrilaterals in user space: (page, quads).
    pub(crate) fn selection_quads(&self, info: &DocInfo) -> Option<(usize, Vec<[f64; 8]>)> {
        let s = self.selection?;
        let text = self.texts.get(&s.page)?;
        let page = info.pages.get(s.page)?;
        let quads: Vec<[f64; 8]> = s.rects(text).into_iter().map(|r| page.view_rect_to_quad(r)).collect();
        (!quads.is_empty()).then_some((s.page, quads))
    }

    /// The source text currently shown in the translation popup, if it is open.
    pub fn translation_original(&self) -> Option<&str> {
        (self.translation.open && !self.translation.original.is_empty())
            .then_some(self.translation.original.as_str())
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection = None;
        self.actions_dismissed = None;
    }

    /// Select text on `page` from glyph `from` to glyph `to` (tests and automation).
    pub fn select_text(&mut self, page: usize, from: usize, to: usize) {
        self.all_text = false;
        self.selection = Some(Selection::run(page, from, to));
    }

    /// Screen position of the centre of glyph `glyph` on `page`, if that page is on screen and its
    /// text layer is loaded (used by UI tests and automation to aim pointer input).
    pub fn glyph_screen_pos(&self, page: usize, glyph: usize) -> Option<Pos2> {
        let (_, xf) = self.screen_xforms.iter().find(|(p, _)| *p == page)?;
        let g = self.texts.get(&page)?.glyphs.get(glyph)?;
        Some(xf.view_rect(g.rect).center())
    }

    /// Selected text, if any (⌘C).
    /// The selected text: of the pages read so far, after Select All of the document (Copy reads
    /// the others, [`DocView::pages_without_text`]).
    pub fn selected_text(&self) -> Option<String> {
        if self.all_text {
            return Some(self.whole_text(&HashMap::new())).filter(|x| !x.is_empty());
        }
        let s = self.selection?;
        let t = self.texts.get(&s.page)?;
        Some(s.text(t)).filter(|x| !x.is_empty())
    }

    /// The selection's page, its text layer and the selected glyphs in copy order, for Copy with
    /// formatting. `None` after Select All of the document and for a column selection, which
    /// copy plain text (a column's rows and tab-separated cells are not paragraphs).
    pub(crate) fn selected_glyphs(&self) -> Option<(usize, Arc<PageText>, Vec<usize>)> {
        if self.all_text {
            return None;
        }
        let s = self.selection?;
        let t = self.texts.get(&s.page)?;
        let glyphs = match s.span {
            Span::Run { anchor, head } => Selection::range(anchor, head).take(t.glyphs.len()).collect(),
            Span::Column { .. } => return None,
        };
        Some((s.page, t.clone(), glyphs))
    }

    /// Select every page without navigating; shared by Organize and the Pages sidebar.
    pub fn select_all_pages(&mut self) -> bool {
        self.selected = (0..self.page_count).collect();
        self.select_anchor = (self.page_count > 0).then_some(self.current);
        !self.selected.is_empty()
    }

    /// Select all words on the current page, or every page in Organize.
    pub fn select_all(&mut self) -> bool {
        if self.organize {
            return self.select_all_pages();
        }
        // Scrolling continuously, Select All takes the whole document's text (#2639); one page at
        // a time, the page shown.
        // Like the page's, true once some text is selected: the pages whose text isn't read yet are
        // read when the selection is copied.
        if self.continuous && self.page_count > 1 {
            self.selection = None;
            self.all_text = true;
            return self.selected_text().is_some();
        }
        self.all_text = false;
        let page = self.current;
        let Some(t) = self.texts.get(&page) else { return false };
        if t.glyphs.is_empty() {
            return false;
        }
        self.selection = Some(Selection::run(page, 0, t.glyphs.len() - 1));
        true
    }

    /// Pages whose text [`DocView::whole_text`] still needs: not read yet, nor failed.
    pub(crate) fn pages_without_text(&self) -> Vec<usize> {
        (0..self.page_count).filter(|p| !self.texts.contains_key(p) && !self.text_failed.contains(p)).collect()
    }

    /// The whole document's text in page order, one page after another on new lines, from the
    /// pages' text read so far and `more` (pages read for this, keyed by page).
    pub(crate) fn whole_text(&self, more: &HashMap<usize, Arc<PageText>>) -> String {
        let mut out = String::new();
        for p in 0..self.page_count {
            let Some(t) = self.texts.get(&p).or_else(|| more.get(&p)) else { continue };
            let text = t.text_of(0..t.glyphs.len());
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text);
        }
        out
    }
}

#[derive(Clone, Copy)]
enum SelectionAction {
    Translate,
    Note,
    Markup(comments::CommentTool),
    Replace,
    Copy,
}

/// Actions on an existing text selection, next to the selected words.
pub(super) fn selection_actions(
    ui: &mut egui::Ui,
    viewport: Rect,
    view: &mut DocView,
    info: &DocInfo,
    prefs: &comments::CommentPrefs,
    can_comment: bool,
    can_copy: bool,
    document: &str,
    favorite_languages: &[(String, String)],
) -> bool {
    if ui.input(|input| input.pointer.is_decidedly_dragging()) {
        return false;
    }
    let Some(selection) = view.selection else {
        view.actions_dismissed = None;
        return false;
    };
    if view.actions_dismissed == Some(selection) {
        return false;
    }
    let Some(text) = view.texts.get(&selection.page) else { return false };
    let Some(xf) = view.page_xform(selection.page) else { return false };
    let Some(last) = selection.rects(text).last().copied() else { return false };
    let selected = selection.text(text);
    if selected.is_empty() {
        return false;
    }
    let rect = xf.view_rect(last);
    if !viewport.intersects(rect) {
        return false;
    }
    let x = rect.left().clamp(viewport.left() + 8.0, (viewport.right() - 520.0).max(viewport.left() + 8.0));
    let y = if rect.bottom() + 48.0 < viewport.bottom() { rect.bottom() + 6.0 } else { (rect.top() - 44.0).max(viewport.top() + 6.0) };
    view.translation.poll();
    if view.translation.open && view.translation.original != selected {
        view.translation.close();
    }
    if view.translation.open {
        view.translation.extra_languages = favorite_languages.to_vec();
        translation_popup(ui, &mut view.translation, view.id, pos2(x, y + 46.0));
        if view.translation.save_requested {
            view.translation.save_requested = false;
            if let (Some(result), Some((page, quads))) = (view.translation.translated.clone(), view.selection_quads(info)) {
                let contents = result;
                view.pending_edit = Some(Edit::AddAnnotation(pdfcraft_engine::NewAnnotation {
                    page,
                    shape: pdfcraft_engine::Shape::TextMarkup { kind: pdfcraft_engine::Markup::Underline, quads },
                    style: pdfcraft_engine::Style { color: [0.0, 0.47, 0.84], opacity: 1.0, width: 1.0, fill: None },
                    contents,
                    author: String::new(),
                }));
                view.translation.close();
                view.clear_selection();
                return false;
            }
        }
    }
    let mut chosen = None;
    egui::Area::new(egui::Id::unique(("text-selection-actions", view.id.0))).order(egui::Order::Foreground).fixed_pos(pos2(x, y)).show(
        ui.ctx(),
        |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (label, action) in [
                        (tl!("Add comment"), SelectionAction::Note),
                        (tl!("Highlight"), SelectionAction::Markup(comments::CommentTool::Highlight)),
                        (tl!("Underline"), SelectionAction::Markup(comments::CommentTool::Underline)),
                        (tl!("Strikethrough"), SelectionAction::Markup(comments::CommentTool::StrikeOut)),
                        (tl!("Replace text"), SelectionAction::Replace),
                    ] {
                        if ui.add_enabled(can_comment, egui::Button::new(label)).clicked() {
                            chosen = Some(action);
                        }
                    }
                    if ui.add_enabled(can_copy, egui::Button::new(tl!("Translate"))).clicked() {
                        chosen = Some(SelectionAction::Translate);
                    }
                    if ui.add_enabled(can_copy, egui::Button::new(tl!("Copy"))).clicked() {
                        chosen = Some(SelectionAction::Copy);
                    }
                });
            });
        },
    );
    let Some(action) = chosen else { return false };
    if matches!(action, SelectionAction::Copy) {
        // Copying closes the bar but keeps the words selected, for ⌘C or a second action.
        view.actions_dismissed = Some(selection);
        return true;
    }
    if matches!(action, SelectionAction::Translate) {
        view.translation.open(selected.to_string(), document.to_string(), selection.page);
        view.translation.start();
        return false;
    }
    let Some((page, quads)) = view.selection_quads(info) else { return false };
    let last = quads.last().copied().unwrap_or_default();
    let at = [last[2].max(last[6]), last[1].max(last[3])];
    view.clear_selection();
    match action {
        SelectionAction::Note | SelectionAction::Replace => {
            if matches!(action, SelectionAction::Replace) {
                view.comments.replace = Some(quads);
            }
            view.comments.composer = Some(comments::Composer {
                page,
                at,
                kind: if matches!(action, SelectionAction::Replace) { comments::ComposerKind::Replace } else { comments::ComposerKind::Note },
                text: String::new(),
                focus: true,
            });
        }
        SelectionAction::Markup(tool) => {
            // Every markup button carries a markup tool; anything else adds nothing.
            let Some(kind) = tool.markup() else { return false };
            view.pending_edit = Some(Edit::AddAnnotation(pdfcraft_engine::NewAnnotation {
                page,
                shape: pdfcraft_engine::Shape::TextMarkup { kind, quads },
                style: prefs.style(tool),
                contents: String::new(),
                author: prefs.author.clone(),
            }));
        }
        SelectionAction::Translate | SelectionAction::Copy => {}
    }
    false
}

fn translation_popup(ui: &mut egui::Ui, view: &mut crate::translation::TranslationView, id: DocId, at: Pos2) {
    let mut changed = false;
    egui::Area::new(egui::Id::new(("translation-popup", id.0))).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(520.0);
            ui.columns(2, |columns| {
                let languages = view.languages();
                let source = view.source.clone();
                egui::ComboBox::from_id_salt(("translation-source", id.0))
                    .width(220.0)
                    .selected_text(languages.iter().find(|(code, _)| *code == source).map_or(source.as_str(), |(_, name)| name.as_str()))
                    .show_ui(&mut columns[0], |ui| {
                        for (code, name) in &languages {
                            changed |= ui.selectable_value(&mut view.source, code.clone(), tl!(name)).changed();
                        }
                    });
                let input_changed =
                    columns[0].add(egui::TextEdit::multiline(&mut view.original).desired_width(f32::INFINITY).desired_rows(5)).changed();
                if input_changed {
                    view.translated = None;
                    view.error = None;
                    view.saved = false;
                }

                let target = view.target.clone();
                egui::ComboBox::from_id_salt(("translation-target", id.0))
                    .width(220.0)
                    .selected_text(languages.iter().find(|(code, _)| *code == target).map_or(target.as_str(), |(_, name)| name.as_str()))
                    .show_ui(&mut columns[1], |ui| {
                        for (code, name) in languages.iter().filter(|(code, _)| code != "auto") {
                            changed |= ui.selectable_value(&mut view.target, code.clone(), tl!(name)).changed();
                        }
                    });
                if let Some(result) = view.translated.as_mut() {
                    crate::translation::show_output(&mut columns[1], result);
                } else if view.loading() {
                    columns[1].add_sized([220.0, 100.0], egui::Label::new(tl!("Translating…")));
                } else if let Some(error) = &view.error {
                    columns[1].colored_label(columns[1].visuals().error_fg_color, error);
                } else {
                    columns[1].add_sized([220.0, 100.0], egui::Label::new(tl!("Translation will appear here")));
                }
            });
            if changed {
                view.start();
            }
            if view.loading() {
                ui.ctx().request_repaint();
            }
            ui.horizontal(|ui| {
                if ui.button(tl!("Translate")).clicked() {
                    view.start();
                }
                if let Some(error) = &view.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.button(tl!("Retry")).clicked() {
                        view.start();
                    }
                }
                if let Some(result) = &view.translated {
                    if ui.button(tl!("Copy")).clicked() {
                        ui.ctx().copy_text(result.clone());
                    }
                    if ui.button(if view.saved { tl!("Saved") } else { tl!("Save") }).clicked() && !view.saved {
                        view.save_requested = true;
                        view.saved = true;
                    }
                }
            });
        });
    });
}
