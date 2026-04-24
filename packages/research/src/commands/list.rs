use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;

use crate::output::Envelope;
use crate::session::{config, event::SessionEvent, layout, log};

const CMD: &str = "research list";

struct Row {
    slug: String,
    topic: String,
    preset: String,
    created_at: chrono::DateTime<chrono::Utc>,
    source_count: u32,
    status: &'static str,
    parent_slug: Option<String>,
    tags: Vec<String>,
}

pub fn run(filter_tag: Option<&str>, tree: bool) -> Envelope {
    let root = layout::research_root();
    if !root.exists() {
        return Envelope::ok(CMD, json!({ "sessions": [] }));
    }

    let entries = match fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read root: {e}")),
    };

    let mut rows: Vec<Row> = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let slug = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) if !s.starts_with('.') => s.to_string(),
            _ => continue,
        };
        if !config::exists(&slug) {
            continue;
        }
        let cfg = match config::read(&slug) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(t) = filter_tag
            && !cfg.tags.iter().any(|x| x == t)
        {
            continue;
        }
        let source_count = log::read_all(&slug)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| matches!(e, SessionEvent::SourceAccepted { .. }))
                    .count() as u32
            })
            .unwrap_or(0);
        let status = if cfg.is_closed() { "closed" } else { "open" };
        rows.push(Row {
            slug: cfg.slug,
            topic: cfg.topic,
            preset: cfg.preset,
            created_at: cfg.created_at,
            source_count,
            status,
            parent_slug: cfg.parent_slug,
            tags: cfg.tags,
        });
    }

    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    if tree {
        return render_tree(rows);
    }
    render_flat(rows)
}

fn render_flat(rows: Vec<Row>) -> Envelope {
    let sessions: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "slug": r.slug,
                "topic": r.topic,
                "preset": r.preset,
                "created_at": r.created_at,
                "source_count": r.source_count,
                "status": r.status,
                "parent_slug": r.parent_slug,
                "tags": r.tags,
            })
        })
        .collect();
    Envelope::ok(CMD, json!({ "sessions": sessions }))
}

fn render_tree(rows: Vec<Row>) -> Envelope {
    // Index by slug + group by parent.
    let slugs: std::collections::HashSet<String> = rows.iter().map(|r| r.slug.clone()).collect();
    let mut children_of: HashMap<String, Vec<&Row>> = HashMap::new();
    let mut roots: Vec<&Row> = Vec::new();
    let mut orphans: Vec<&Row> = Vec::new();
    for r in &rows {
        match &r.parent_slug {
            Some(p) if slugs.contains(p) => {
                children_of.entry(p.clone()).or_default().push(r);
            }
            Some(_) => orphans.push(r),
            None => roots.push(r),
        }
    }

    // Build nested JSON tree + plain-text ASCII rendering.
    fn node_json(r: &Row, children: &HashMap<String, Vec<&Row>>) -> Value {
        let kids: Vec<Value> = children
            .get(&r.slug)
            .map(|v| v.iter().map(|c| node_json(c, children)).collect())
            .unwrap_or_default();
        json!({
            "slug": r.slug,
            "topic": r.topic,
            "preset": r.preset,
            "created_at": r.created_at,
            "source_count": r.source_count,
            "status": r.status,
            "parent_slug": r.parent_slug,
            "tags": r.tags,
            "children": kids,
        })
    }

    let tree_json: Vec<Value> = roots.iter().map(|r| node_json(r, &children_of)).collect();

    // Plain-text ASCII rendering — printed via envelope's non-JSON path.
    // We also dump it to stderr-neutral stdout eagerly so plain-text users
    // see structure regardless of the data payload form.
    fn print_row(indent: &str, r: &Row) {
        println!(
            "{indent}- {slug}  [{status}, {count} srcs{parent}{tags}] {topic}",
            slug = r.slug,
            status = r.status,
            count = r.source_count,
            parent = r
                .parent_slug
                .as_deref()
                .map(|p| format!(", parent={p}"))
                .unwrap_or_default(),
            tags = if r.tags.is_empty() {
                String::new()
            } else {
                format!(", tags={:?}", r.tags)
            },
            topic = r.topic,
        );
    }
    fn print_subtree(r: &Row, depth: usize, children: &HashMap<String, Vec<&Row>>) {
        let indent = "  ".repeat(depth);
        print_row(&indent, r);
        if let Some(kids) = children.get(&r.slug) {
            for c in kids {
                print_subtree(c, depth + 1, children);
            }
        }
    }
    for r in &roots {
        print_subtree(r, 0, &children_of);
    }
    if !orphans.is_empty() {
        println!("\n(orphaned — parent missing)");
        for r in &orphans {
            print_row("  ", r);
            let missing_parent = r.parent_slug.clone().unwrap_or_default();
            println!("    (parent missing: {missing_parent})");
        }
    }

    let orphans_json: Vec<Value> = orphans
        .iter()
        .map(|r| {
            json!({
                "slug": r.slug,
                "topic": r.topic,
                "parent_slug": r.parent_slug,
                "parent_missing": true,
                "tags": r.tags,
            })
        })
        .collect();

    Envelope::ok(
        CMD,
        json!({
            "tree": tree_json,
            "orphans": orphans_json,
        }),
    )
}
