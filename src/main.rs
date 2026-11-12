//! Forge IDE — a lightweight code editor written in Rust.

mod ai;
mod app;
mod cli;
mod commands;
mod editor;
mod editorconfig;
mod extensions;
mod fuzzy;
mod git;
mod github;
mod history;
mod keymap;
mod lsp;
mod palette;
mod panes;
mod search;
mod settings;
mod task;
mod terminal;
mod theme;
mod ui;
mod updater;
mod watcher;
mod windows;
mod workspace;

use app::ForgeApp;

fn main() -> eframe::Result {
    // A folder given on the command line; otherwise the last one opened,
    // unless this is a new empty window.
    let arg = std::env::args_os().nth(1);
    let new_window = arg
        .as_deref()
        .is_some_and(|a| a == windows::NEW_WINDOW_FLAG);
    let root = arg.filter(|_| !new_window).map(std::path::PathBuf::from);

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
        Box::new(move |cc| Ok(Box::new(ForgeApp::new(cc, root, new_window)))),
    )
}
