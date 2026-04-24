use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::output::Envelope;
use crate::session::{config, layout};

const CMD: &str = "research series";

struct Member {
    slug: String,
    topic: String,
    preset: String,
    created_at: chrono::DateTime<chrono::Utc>,
    report_html: Option<String>,
    first_finding: Option<String>,
}

pub fn run(tag: &str, open: bool) -> Envelope {
    let root = layout::research_root();

    let mut members: Vec<Member> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen_slugs: HashSet<String> = HashSet::new();

    for discovery_root in layout::research_roots_for_discovery() {
        if !discovery_root.exists() {
            continue;
        }
        let entries = match fs::read_dir(&discovery_root) {
            Ok(e) => e,
            Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read root: {e}")),
        };
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            let slug = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) if !s.starts_with('.') => s.to_string(),
                _ => continue,
            };
            if !seen_slugs.insert(slug.clone()) || !config::exists(&slug) {
                continue;
            }
            let cfg = match config::read(&slug) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !cfg.tags.iter().any(|t| t == tag) {
                continue;
            }

            let html_path = layout::session_report_html(&slug);
            let json_path = layout::session_report_json(&slug);

            let (report_html, first_finding) = if html_path.exists() && json_path.exists() {
                (
                    Some(format!("{slug}/report.html")),
                    extract_first_finding(&json_path),
                )
            } else {
                warnings.push(format!("session '{slug}' not synthesized"));
                (None, None)
            };

            members.push(Member {
                slug: cfg.slug,
                topic: cfg.topic,
                preset: cfg.preset,
                created_at: cfg.created_at,
                report_html,
                first_finding,
            });
        }
    }

    members.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    if members.is_empty() {
        return Envelope::fail(CMD, "TAG_NOT_FOUND", format!("no sessions tagged '{tag}'"))
            .with_context(json!({ "tag": tag }));
    }

    let doc = build_index_doc(tag, &members);
    if let Err(e) = fs::create_dir_all(&root) {
        return Envelope::fail(CMD, "IO_ERROR", format!("create root: {e}"));
    }
    let index_json_path = root.join(format!("series-{tag}.json"));
    let index_html_path = root.join(format!("series-{tag}.html"));

    let serialized = match serde_json::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("serialize: {e}")),
    };
    if let Err(e) = fs::write(&index_json_path, &serialized) {
        return Envelope::fail(CMD, "IO_ERROR", format!("write series json: {e}"));
    }

    let mut render_error: Option<String> = None;
    if let Err(e) = render_html(&index_json_path, &index_html_path) {
        render_error = Some(e);
    }

    if open && render_error.is_none() && !should_skip_open() {
        let spawn_result = if cfg!(target_os = "macos") {
            Command::new("open").arg(&index_html_path).spawn()
        } else {
            Command::new("xdg-open").arg(&index_html_path).spawn()
        };
        if let Err(e) = spawn_result {
            eprintln!("⚠ open failed: {e}");
        }
    }

    let data = json!({
        "tag": tag,
        "member_count": members.len(),
        "members": members.iter().map(|m| json!({
            "slug": m.slug,
            "topic": m.topic,
            "preset": m.preset,
            "created_at": m.created_at,
            "report_html": m.report_html,
            "first_finding": m.first_finding,
        })).collect::<Vec<_>>(),
        "index_json_path": filename(&index_json_path),
        "index_html_path": if render_error.is_none() { Some(filename(&index_html_path)) } else { None },
        "warnings": warnings,
    });

    if let Some(err) = render_error {
        return Envelope::fail(CMD, "RENDER_FAILED", err)
            .with_context(json!({ "tag": tag }))
            .with_details(data);
    }

    Envelope::ok(CMD, data).with_context(json!({ "tag": tag }))
}

fn extract_first_finding(json_path: &Path) -> Option<String> {
    let text = fs::read_to_string(json_path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let children = v.get("children")?.as_array()?;
    let findings_section = children.iter().find(|c| {
        c.get("props").and_then(|p| p.get("title")) == Some(&Value::String("Key Findings".into()))
    })?;
    let list = findings_section.get("children")?.as_array()?.first()?;
    let items = list.get("props")?.get("items")?.as_array()?;
    items
        .first()?
        .get("title")
        .and_then(|t| t.as_str())
        .map(String::from)
}

fn build_index_doc(tag: &str, members: &[Member]) -> Value {
    let mut children = vec![json!({
        "type": "BrandHeader",
        "props": {
            "badge": format!("Research Series: {tag}"),
            "poweredBy": "Actionbook / research CLI"
        }
    })];

    let items: Vec<Value> = members
        .iter()
        .map(|m| {
            let link = m
                .report_html
                .as_ref()
                .map(|p| format!("<a href=\"{p}\">{slug}</a>", slug = m.slug))
                .unwrap_or_else(|| format!("{} (no report yet)", m.slug));
            let finding = m
                .first_finding
                .as_ref()
                .map(|f| format!(" — key finding: **{f}**"))
                .unwrap_or_default();
            json!({
                "badge": m.slug.clone(),
                "title": m.topic,
                "description": format!(
                    "{link}{finding}  _(preset: {preset}, created: {ts})_",
                    preset = m.preset,
                    ts = m.created_at.to_rfc3339(),
                ),
            })
        })
        .collect();

    children.push(json!({
        "type": "Section",
        "props": { "title": format!("Members ({})", members.len()), "icon": "link" },
        "children": [
            {
                "type": "ContributionList",
                "props": { "items": items }
            }
        ]
    }));

    children.push(json!({
        "type": "BrandFooter",
        "props": {
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "attribution": "Powered by Actionbook + postagent",
            "disclaimer": "Generated by `research series`."
        }
    }));

    json!({
        "type": "Report",
        "props": { "theme": "auto" },
        "children": children,
    })
}

fn render_html(json_path: &Path, html_path: &Path) -> Result<PathBuf, String> {
    let bin = std::env::var("JSON_UI_BIN").unwrap_or_else(|_| "json-ui".to_string());
    let output = Command::new(&bin)
        .arg("render")
        .arg(json_path)
        .arg("-o")
        .arg(html_path)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "json-ui binary '{bin}' not found on PATH (install json-ui or set JSON_UI_BIN)"
            ),
            _ => format!("spawn json-ui: {e}"),
        })?;
    if !output.status.success() {
        return Err(format!(
            "json-ui render exit {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(html_path.to_path_buf())
}

fn should_skip_open() -> bool {
    if std::env::var("SYNTHESIZE_NO_OPEN").is_ok() || std::env::var("CI").is_ok() {
        return true;
    }
    !std::io::stdin().is_terminal()
}

fn filename(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}
