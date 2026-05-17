//! Report assembly. Two flavors:
//!
//! * `builder` — legacy synthesize path: session → json-ui `report.json`,
//!   consumed by the external `json-ui` renderer.
//! * `template` — new editorial path: session → embedded HTML template with
//!   placeholder substitution. Produces `report-rich.html` directly, no
//!   json-ui dependency.

pub mod bilingual;
pub mod brief_md;
pub mod builder;
pub mod markdown;
pub mod pdf_local;
pub mod sources;
pub mod template;
pub mod wiki_render;
