//! The activity bar: a narrow strip on the far left that switches the
//! sidebar between the explorer, project search, source control and pull
//! requests.

use egui::{Sense, Stroke, Ui, vec2};

use super::icons::{self, Icon};
use crate::theme::UiColors;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SidebarView {
    #[default]
    Explorer,
    Search,
    SourceControl,
    PullRequests,
}

impl SidebarView {
    pub const ALL: [SidebarView; 4] = [
        Self::Explorer,
        Self::Search,
        Self::SourceControl,
        Self::PullRequests,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Explorer => "Explorer",
            Self::Search => "Search",
            Self::SourceControl => "Source Control",
            Self::PullRequests => "Pull Requests",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Explorer => Icon::Files,
            Self::Search => Icon::Search,
            Self::SourceControl => Icon::Branch,
            Self::PullRequests => Icon::PullRequest,
        }
    }
}

pub const WIDTH: f32 = 48.0;

/// Draws the bar. Clicking the active view hides the sidebar; clicking
/// another view shows it. `badges` gives a count per view (0 = none).
pub fn show(
    ui: &mut Ui,
    view: &mut SidebarView,
    sidebar_open: &mut bool,
    badges: impl Fn(SidebarView) -> usize,
    colors: &UiColors,
) {
    ui.add_space(6.0);
    for v in SidebarView::ALL {
        let active = *view == v && *sidebar_open;
        let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 44.0), Sense::click());
        let hovered = response.hovered();
        let color = if active || hovered {
            colors.text.0
        } else {
            colors.text_muted.0
        };
        if active {
            // Accent bar on the left edge marks the active view.
            ui.painter().line_segment(
                [
                    rect.left_top() + vec2(1.0, 8.0),
                    rect.left_bottom() + vec2(1.0, -8.0),
                ],
                Stroke::new(2.5, colors.accent.0),
            );
        }
        let icon_rect = egui::Rect::from_center_size(rect.center(), vec2(24.0, 24.0));
        icons::paint(ui.painter(), icon_rect, v.icon(), color);
        let count = badges(v);
        if count > 0 {
            let text = if count > 99 {
                "99+".to_owned()
            } else {
                count.to_string()
            };
            icons::badge(
                ui.painter(),
                icon_rect.right_bottom() + vec2(-1.0, -2.0),
                &text,
                colors.accent.0,
                colors.accent_text.0,
            );
        }
        let response = response.on_hover_text(v.title());
        if response.clicked() {
            if *view == v {
                *sidebar_open = !*sidebar_open;
            } else {
                *view = v;
                *sidebar_open = true;
            }
        }
    }
}
