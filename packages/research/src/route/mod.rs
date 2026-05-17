//! URL routing: load preset (TOML) → classify URL → build command template.
//!
//! See `research-route-toml-presets.spec.md`.

pub mod rules;

pub use rules::{
    Classification, Executor, Preset, PresetError, PresetSubCode, ResolvedPart, Route, classify,
    load_preset,
};
