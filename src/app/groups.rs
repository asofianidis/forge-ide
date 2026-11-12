//! Editor groups: the editor area can be split into side-by-side groups,
//! each with its own tabs. A tab is a view onto an open document; the same
//! document can be open in several groups (or twice side by side), each view
//! with its own cursors, folds and scroll position.
//!
//! `ForgeApp::documents` holds each open file once. A document stays open
//! while any tab shows it; closing its last tab closes it (asking about
//! unsaved changes first).

use super::ForgeApp;
use crate::editor::Document;
use crate::editor::view::EditorView;

/// The most groups side by side.
pub(super) const MAX_GROUPS: usize = 3;

pub(super) struct EditorGroup {
    pub tabs: Vec<EditorView>,
    /// Index into `tabs` of the selected tab.
    pub active: usize,
    /// Share of the editor area's width.
    pub weight: f32,
}

impl EditorGroup {
    pub fn new(tabs: Vec<EditorView>) -> Self {
        Self {
            tabs,
            active: 0,
            weight: 1.0,
        }
    }

    pub fn active_view(&self) -> Option<&EditorView> {
        self.tabs.get(self.active)
    }
}

impl ForgeApp {
    /// Index into `documents` of the document with `id`.
    pub(super) fn doc_by_id(&self, id: u64) -> Option<usize> {
        self.documents.iter().position(|d| d.id == id)
    }

    /// Fixes up the focused group and active tab after tabs change, and
    /// points `active` at the focused tab's document.
    pub(super) fn sync_active(&mut self) {
        // Drop tabs whose document is gone, then empty groups.
        let ids: Vec<u64> = self.documents.iter().map(|d| d.id).collect();
        for g in &mut self.groups {
            g.tabs.retain(|v| ids.contains(&v.doc));
        }
        if self.groups.len() > 1 {
            let mut i = 0;
            while i < self.groups.len() {
                if self.groups[i].tabs.is_empty() && self.groups.len() > 1 {
                    self.groups.remove(i);
                    if self.group > i {
                        self.group -= 1;
                    }
                } else {
                    i += 1;
                }
            }
        }
        if self.groups.is_empty() {
            self.groups.push(EditorGroup::new(Vec::new()));
        }
        self.group = self.group.min(self.groups.len() - 1);
        if self.documents.is_empty() {
            self.documents.push(Document::untitled());
        }
        if self.groups[self.group].tabs.is_empty() {
            // Show something: the first document.
            let id = self.documents[0].id;
            self.groups[self.group].tabs.push(EditorView::new(id));
        }
        for g in &mut self.groups {
            g.active = g.active.min(g.tabs.len().saturating_sub(1));
        }
        // Documents no tab shows any more are closed.
        let shown: Vec<u64> = self
            .groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(|v| v.doc))
            .collect();
        self.documents.retain(|d| shown.contains(&d.id));
        let g = &self.groups[self.group];
        self.active = g
            .active_view()
            .and_then(|v| self.documents.iter().position(|d| d.id == v.doc))
            .unwrap_or(0);
    }

    pub(super) fn focused_view(&self) -> Option<&EditorView> {
        self.groups.get(self.group)?.active_view()
    }

    /// The focused view and its document, both mutable.
    pub(super) fn focused_doc_view(&mut self) -> Option<(&mut Document, &mut EditorView)> {
        let g = self.groups.get_mut(self.group)?;
        let view = g.tabs.get_mut(g.active)?;
        let doc = self.documents.iter_mut().find(|d| d.id == view.doc)?;
        Some((doc, view))
    }

    /// Shows document `idx` in the focused group, in its existing tab there
    /// or a new one.
    pub(super) fn show_document(&mut self, idx: usize) {
        let Some(id) = self.documents.get(idx).map(|d| d.id) else {
            return;
        };
        // Panes only live in the first group; showing a file there hides them.
        if self.group == 0 {
            self.active_pane = None;
        }
        let g = &mut self.groups[self.group];
        match g.tabs.iter().position(|v| v.doc == id) {
            Some(t) => g.active = t,
            None => {
                let at = (g.active + 1).min(g.tabs.len());
                g.tabs.insert(at, EditorView::new(id));
                g.active = at;
            }
        }
        self.sync_active();
        // Type straight into the file just shown (once any picker is gone).
        self.focus_editor = true;
    }

    /// Selects tab `tab` of group `group` and focuses that group.
    pub(super) fn select_tab(&mut self, group: usize, tab: usize) {
        if let Some(g) = self.groups.get_mut(group) {
            g.active = tab.min(g.tabs.len().saturating_sub(1));
            self.group = group;
            if group == 0 {
                self.active_pane = None;
            }
        }
        self.sync_active();
    }

    /// Whether some other tab than `(group, tab)` shows the same document.
    fn shown_elsewhere(&self, group: usize, tab: usize) -> bool {
        let Some(id) = self.groups[group].tabs.get(tab).map(|v| v.doc) else {
            return false;
        };
        self.groups.iter().enumerate().any(|(gi, g)| {
            g.tabs
                .iter()
                .enumerate()
                .any(|(ti, v)| v.doc == id && (gi, ti) != (group, tab))
        })
    }

    /// Closes a tab, first asking what to do with unsaved changes when it is
    /// the document's last tab.
    pub(super) fn close_tab_in(&mut self, group: usize, tab: usize) {
        let Some(view) = self.groups.get(group).and_then(|g| g.tabs.get(tab)) else {
            return;
        };
        let dirty = self
            .doc_by_id(view.doc)
            .is_some_and(|i| self.documents[i].is_dirty());
        if dirty && !self.shown_elsewhere(group, tab) {
            self.pending_close = Some(super::PendingClose::Tab(view.doc));
        } else {
            self.remove_tab_in(group, tab);
        }
    }

    /// Removes a tab without asking; its document closes if no other tab shows it.
    pub(super) fn remove_tab_in(&mut self, group: usize, tab: usize) {
        let Some(g) = self.groups.get_mut(group) else {
            return;
        };
        if tab >= g.tabs.len() {
            return;
        }
        g.tabs.remove(tab);
        if g.active > tab || g.active >= g.tabs.len() {
            g.active = g.active.saturating_sub(1);
        }
        if g.tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(group);
            if self.group >= group && self.group > 0 {
                self.group -= 1;
            }
        }
        self.sync_active();
    }

    /// Closes document `idx` and every tab showing it, without asking.
    pub(super) fn close_document(&mut self, idx: usize) {
        if idx >= self.documents.len() {
            return;
        }
        self.documents.remove(idx);
        self.sync_active();
    }

    /// Opens a second view of the focused tab in a new group to the right
    /// (or in the next group, when there are already as many as fit).
    pub(super) fn split_editor(&mut self) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let mut copy = EditorView::new(view.doc);
        copy.sel = view.sel.clone();
        copy.reveal = Some(crate::editor::view::Reveal::Center);
        let target = if self.groups.len() < MAX_GROUPS {
            self.groups
                .insert(self.group + 1, EditorGroup::new(Vec::new()));
            self.group + 1
        } else {
            (self.group + 1) % self.groups.len()
        };
        let g = &mut self.groups[target];
        match g.tabs.iter().position(|v| v.doc == copy.doc) {
            Some(t) => g.active = t,
            None => {
                g.tabs.push(copy);
                g.active = g.tabs.len() - 1;
            }
        }
        self.focus_group(target);
    }

    /// Moves the focused tab into the next group (creating one if this is
    /// the only group).
    pub(super) fn move_tab_to_next_group(&mut self) {
        let from = self.group;
        if self.groups.len() == 1 {
            if self.groups[0].tabs.len() < 2 {
                self.status = "Open another tab first, or use Split Editor".to_owned();
                return;
            }
            self.groups.push(EditorGroup::new(Vec::new()));
        }
        let to = (from + 1) % self.groups.len();
        let g = &mut self.groups[from];
        let view = g.tabs.remove(g.active);
        if g.active >= g.tabs.len() {
            g.active = g.tabs.len().saturating_sub(1);
        }
        let target = &mut self.groups[to];
        match target.tabs.iter().position(|v| v.doc == view.doc) {
            Some(t) => target.active = t,
            None => {
                target.tabs.push(view);
                target.active = target.tabs.len() - 1;
            }
        }
        let to_after = if self.groups[from].tabs.is_empty() && to > from {
            to - 1
        } else {
            to
        };
        self.sync_active();
        self.focus_group(to_after.min(self.groups.len() - 1));
    }

    /// Makes group `g` the focused one and gives its editor keyboard focus.
    pub(super) fn focus_group(&mut self, g: usize) {
        if g >= self.groups.len() {
            return;
        }
        self.group = g;
        self.sync_active();
        if let Some(id) = self.focused_view().map(|v| v.id) {
            self.egui_ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Gives the focused editor keyboard focus when asked to, once no dialog
    /// is open (a modal would keep the focus from moving below it).
    pub(super) fn apply_editor_focus(&mut self, ctx: &egui::Context) {
        if !self.focus_editor || self.picker.open || self.go_to_line.is_some() {
            return;
        }
        self.focus_editor = false;
        if let Some(id) = self.focused_view().map(|v| v.id) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Follows keyboard focus: the group whose editor has focus is focused.
    pub(super) fn follow_editor_focus(&mut self, ctx: &egui::Context) {
        let Some(focused) = ctx.memory(|m| m.focused()) else {
            return;
        };
        for (gi, g) in self.groups.iter().enumerate() {
            if let Some(ti) = g.tabs.iter().position(|v| v.id == focused) {
                if gi != self.group || ti != g.active {
                    self.group = gi;
                    self.groups[gi].active = ti;
                    self.sync_active();
                }
                return;
            }
        }
    }

    /// The tab and group order of the open documents, for cycling tabs.
    pub(super) fn tab_count(&self) -> usize {
        let g = &self.groups[self.group];
        g.tabs.len() + if self.group == 0 { self.panes.len() } else { 0 }
    }
}
