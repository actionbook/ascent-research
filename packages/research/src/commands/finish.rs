use serde_json::json;

use crate::commands::{audit, coverage, synthesize};
use crate::output::Envelope;

const CMD: &str = "research finish";

pub fn run(slug: &str, open: bool, bilingual: bool) -> Envelope {
    let coverage_env = coverage::run(Some(slug));
    if !coverage_env.ok {
        return stage_error("coverage", coverage_env);
    }
    if coverage_env.data["report_ready"] != json!(true) {
        return Envelope::fail(
            CMD,
            "REPORT_NOT_READY",
            "coverage failed; finish stopped before synthesize",
        )
        .with_context(json!({ "session": slug }))
        .with_details(json!({
            "stage": "coverage",
            "coverage": coverage_env.data,
        }));
    }

    let synthesis_env = synthesize::run(Some(slug), false, open, bilingual);
    if !synthesis_env.ok {
        return stage_error("synthesize", synthesis_env);
    }

    let audit_env = audit::run(Some(slug));
    if !audit_env.ok {
        return stage_error("audit", audit_env);
    }
    if audit_env.data["audit_status"] != json!("complete") {
        return Envelope::fail(CMD, "AUDIT_INCOMPLETE", "audit did not complete")
            .with_context(json!({ "session": slug }))
            .with_details(json!({
                "stage": "audit",
                "coverage": coverage_env.data,
                "synthesis": synthesis_env.data,
                "audit": audit_env.data,
            }));
    }

    Envelope::ok(
        CMD,
        json!({
            "coverage": coverage_env.data,
            "synthesis": synthesis_env.data,
            "audit": audit_env.data,
        }),
    )
    .with_context(json!({ "session": slug }))
}

fn stage_error(stage: &str, env: Envelope) -> Envelope {
    let code = env
        .error
        .as_ref()
        .map(|e| e.code.as_str())
        .unwrap_or("STAGE_FAILED")
        .to_string();
    let message = env
        .error
        .as_ref()
        .map(|e| e.message.clone())
        .unwrap_or_else(|| format!("{stage} failed"));
    Envelope::fail(CMD, &code, message).with_details(json!({
        "stage": stage,
        "envelope": env,
    }))
}
