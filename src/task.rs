//! Tiny helper for running work on a background thread and picking up the
//! result from the UI thread without blocking.

use std::sync::mpsc::{Receiver, channel};

/// The eventual result of a closure running on a background thread.
pub struct Pending<T> {
    rx: Receiver<T>,
}

impl<T: Send + 'static> Pending<T> {
    /// Runs `f` on a new thread and repaints `ctx` when it finishes.
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        Self { rx }
    }
}

impl<T> Pending<T> {
    /// The result, once it is ready. A worker that panicked never yields.
    pub fn try_take(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// Takes the result out of an optional pending slot, clearing the slot when
/// the work is done.
pub fn poll<T>(slot: &mut Option<Pending<T>>) -> Option<T> {
    let value = slot.as_ref()?.try_take()?;
    *slot = None;
    Some(value)
}
