use serde_json::json;

use crate::output::Envelope;

const CMD: &str = "research finish";

pub fn run(slug: &str, _open: bool, _bilingual: bool) -> Envelope {
    Envelope::fail(CMD, "NOT_IMPLEMENTED", "finish is not yet implemented")
        .with_context(json!({ "session": slug }))
}
