//! The egui user interface.
mod alerts_page;
mod app;
mod charts;
mod dashboard;
mod hardware;
mod info;
mod mini;
mod pages;
mod processes;
mod settings_page;
mod storage;
mod theme;
mod widgets;

pub use app::{run, Options, Tab};
