//! Forge IDE — a lightweight code editor written in Rust.

mod app;
mod cli;
mod editor;
mod git;
mod github;
mod history;
mod lsp;
mod panes;
mod settings;
mod task;
mod theme;
mod ui;
mod updater;
mod workspace;

use app::ForgeApp;

fn main() -> eframe::Result {
    let root = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Forge IDE")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Forge IDE",
        options,
        Box::new(move |cc| Ok(Box::new(ForgeApp::new(cc, root)))),
    )
}
