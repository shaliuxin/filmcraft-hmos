//! OHOS-only stand-in for the `eframe` crate.
//!
//! `filmcraft-ui-egui` (upstream, untouched) is written against the real `eframe` fork, whose
//! only touchpoints here are:
//!   * `eframe::wgpu`        — a plain re-export of the `wgpu` crate
//!   * `eframe::egui_wgpu`   — a plain re-export of the `egui-wgpu` crate
//!   * `eframe::Frame`       — only `set_ui_language(..)` is called on it
//!   * `impl eframe::App for FilmcraftApp` — the trait's `raw_input_hook` / `logic` / `ui` / `on_exit`
//!
//! The real eframe pulls in `winit` (no OpenHarmony backend, `compile_error!`), so we replace it,
//! only for this OHOS build, with this shim that has no windowing dependency. The egui / wgpu
//! objects are still owned by the adapter (native/rust), exactly as in WordCraft.
#![allow(dead_code)]

pub use egui_wgpu;
pub use wgpu;

use egui::{Context, RawInput, Ui};

/// Minimal `eframe::Frame`. `set_ui_language` is a no-op here: the adapter already drives the
/// interface language through `ctx` / the app's own `hooks.system_languages`, and `set_ui_language`
/// only ever forwards to `ctx.set_language`, which a hidden-context `Frame` cannot touch anyway.
#[derive(Clone, Default)]
pub struct Frame;

impl Frame {
    pub fn new() -> Self {
        Self
    }

    pub fn set_ui_language(&self, _lang: Option<&str>) {}
}

/// Minimal `eframe::App` trait with the four methods `FilmcraftApp` implements. Every method has
/// a default body so the trait can be implemented partially, matching upstream's usage.
pub trait App {
    fn raw_input_hook(&mut self, _ctx: &Context, _raw_input: &mut RawInput) {}
    fn logic(&mut self, _ctx: &Context, _frame: &mut Frame) {}
    fn ui(&mut self, _ui: &mut Ui, _frame: &mut Frame) {}
    fn on_exit(&mut self) {}
}
