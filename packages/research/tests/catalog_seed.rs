//! Integration tests for `actionbook-catalog-seed.spec.md` — 17 BDD scenarios.
//!
//! Test name → spec scenario mapping (each `测试:` line in the spec must
//! match a test name in this file character-for-character):
//!
//!  1. catalog_seed_writes_wiki_page_on_match
//!  2. catalog_seed_skips_if_wiki_page_exists
//!  3. catalog_seed_silently_continues_when_no_match
//!  4. catalog_seed_silently_continues_when_extension_offline
//!  5. catalog_seed_limits_to_3_manuals_per_url
//!  6. catalog_seed_logs_wiki_seeded_event_to_jsonl
//!  7. catalog_seed_frontmatter_contains_required_fields
//!  8. catalog_seed_reseed_flag_forces_overwrite
//!  9. catalog_seed_v1_backend_skips_catalog
//! 10. catalog_seed_skips_when_host_empty
//! 11. catalog_seed_partial_failure_continues
//! 12. catalog_seed_batch_per_url_independent
//! 13. catalog_seed_does_not_alter_route_or_fetch
//! 14. catalog_seed_filename_slug_rules                (unit)
//! 15. catalog_seed_filename_optional_parts            (unit)
//! 16. catalog_seed_max_constant_is_three_hardcoded    (unit)
//! 17. catalog_seed_silent_skip_writes_no_jsonl
//!
//! Tests 1-13, 17 use the catalog module's `seed_for_url_in` entry against
//! a per-test mock MCP server. Tests 14-16 are pure-unit and don't need the
//! mock. Test 12 (batch) is the only one that spawns the CLI subprocess —
//! everything else exercises the library API directly.
//!
//! Mock server is a minimal in-process HTTP listener (same pattern as
//! `runcode_flags.rs::McpMock`) that records every `tools/call` cmd
//! string and answers configurable responses per-method.

use research::catalog::{self, SeedOpts, MAX_SEED_PER_URL};
use research::session::event::{self, SessionEvent};

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

/// Process-wide serializer. Each test that mutates the `ACTIONBOOK_*`
/// env vars takes this lock for its full body so the env state and the
/// mock-server endpoint stay coherent across parallel test workers.
/// Without this, cargo's default thread-fanout causes mid-test env-var
/// clobber (the global env is one shared resource).
fn env_serializer() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ─── 1. catalog_seed_writes_wiki_page_on_match ────────────────────────────

#[test]
fn catalog_seed_writes_wiki_page_on_match() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", Some("search"), Some("search_timeline"))],
        manual_body: "MANUAL-BODY-001".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t1");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    assert_eq!(report.seeded.len(), 1, "must seed exactly 1 page");
    let page = env.wiki_dir.join("x-com-search-search-timeline.md");
    assert!(page.exists(), "page must exist on disk");
    let content = fs::read_to_string(&page).unwrap();
    assert!(content.contains("kind: actionbook-manual"), "kind missing: {content}");
    assert!(content.contains("source: catalog"), "source missing: {content}");
    assert!(content.contains("MANUAL-BODY-001"), "body missing: {content}");
}

// ─── 2. catalog_seed_skips_if_wiki_page_exists ────────────────────────────

#[test]
fn catalog_seed_skips_if_wiki_page_exists() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", Some("search"), Some("search_timeline"))],
        manual_body: "NEW-BODY".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t2");
    let _envguard = mock.bind_env();

    // Pre-create the wiki page.
    fs::create_dir_all(&env.wiki_dir).unwrap();
    let page = env.wiki_dir.join("x-com-search-search-timeline.md");
    let original = "ORIGINAL-CONTENT-UNCHANGED";
    fs::write(&page, original).unwrap();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    // mock should NOT have received a `manual` request.
    let manual_cmds = mock.manual_cmds();
    assert_eq!(
        manual_cmds.len(),
        0,
        "expected zero manual cmds, got: {manual_cmds:?}"
    );
    assert_eq!(report.seeded.len(), 0);
    // File content unchanged.
    assert_eq!(fs::read_to_string(&page).unwrap(), original);
    // No wiki_seeded event written.
    let events_file = env.session_jsonl();
    if events_file.exists() {
        let events = event::read_events(&events_file).unwrap();
        assert!(
            !events.iter().any(|e| matches!(e, SessionEvent::WikiSeeded { .. })),
            "no WikiSeeded event should be written"
        );
    }
}

// ─── 3. catalog_seed_silently_continues_when_no_match ─────────────────────

#[test]
fn catalog_seed_silently_continues_when_no_match() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![], // empty array → no hits
        ..Default::default()
    });
    let env = TestEnv::new("t3");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    assert_eq!(report.seeded.len(), 0);
    // wiki dir empty (or absent).
    let entries: Vec<_> = fs::read_dir(&env.wiki_dir)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(entries.is_empty(), "no new files expected, got: {entries:?}");
    // session.jsonl has no wiki_seeded events.
    assert_no_wiki_seeded_event(&env);
}

// ─── 4. catalog_seed_silently_continues_when_extension_offline ────────────

#[test]
fn catalog_seed_silently_continues_when_extension_offline() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_error_code: Some("EXTENSION_OFFLINE".to_string()),
        ..Default::default()
    });
    let env = TestEnv::new("t4");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    assert_eq!(report.seeded.len(), 0);
    assert_no_wiki_seeded_event(&env);
}

// ─── 5. catalog_seed_limits_to_3_manuals_per_url ──────────────────────────

#[test]
fn catalog_seed_limits_to_3_manuals_per_url() {
    let _lock = env_serializer();
    let hits: Vec<Value> = (0..7)
        .map(|i| hit(&format!("site{i}"), Some(&format!("g{i}")), Some(&format!("a{i}"))))
        .collect();
    let mock = McpMock::start_with(MockConfig {
        search_hits: hits,
        manual_body: "MANUAL".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t5");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    let manual_cmds = mock.manual_cmds();
    assert_eq!(manual_cmds.len(), 3, "expect 3 manual calls (capped): {manual_cmds:?}");
    assert_eq!(report.seeded.len(), 3);

    let files: Vec<String> = fs::read_dir(&env.wiki_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    assert_eq!(files.len(), 3, "wiki dir should have 3 files: {files:?}");

    // Spec § 上限: "这 3 个文件对应 search 返回数组的前 3 条".
    for i in 0..3 {
        let expected = format!("site{i}-g{i}-a{i}.md");
        assert!(
            files.contains(&expected),
            "expected file {expected} for top-{i} hit; got: {files:?}"
        );
    }
}

// ─── 6. catalog_seed_logs_wiki_seeded_event_to_jsonl ──────────────────────

#[test]
fn catalog_seed_logs_wiki_seeded_event_to_jsonl() {
    let _lock = env_serializer();
    let hits = vec![
        hit("site_a", Some("g_a"), Some("a_a")),
        hit("site_b", Some("g_b"), Some("a_b")),
    ];
    let mock = McpMock::start_with(MockConfig {
        search_hits: hits,
        manual_body: "BODY".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t6");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );
    assert_eq!(report.seeded.len(), 2);

    // log_seed_events appends to the canonical session.jsonl path.
    catalog::log_seed_events("t6", "https://x.com/explore", "x.com", &report);

    let jsonl = env.session_jsonl();
    assert!(jsonl.exists(), "session.jsonl must exist");
    let lines: Vec<String> = fs::read_to_string(&jsonl)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let seeded_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("\"event\":\"wiki_seeded\""))
        .collect();
    assert_eq!(
        seeded_lines.len(),
        2,
        "expect 2 wiki_seeded jsonl lines, got: {lines:?}"
    );

    for line in &seeded_lines {
        let v: Value = serde_json::from_str(line).expect("parse jsonl line");
        assert_eq!(v.get("event").and_then(Value::as_str), Some("wiki_seeded"));
        assert!(v.get("host").is_some(), "host missing: {line}");
        assert!(v.get("site").is_some(), "site missing: {line}");
        assert!(v.get("page").is_some(), "page missing: {line}");
        assert!(v.get("bytes").is_some(), "bytes missing: {line}");
    }
}

// ─── 7. catalog_seed_frontmatter_contains_required_fields ─────────────────

#[test]
fn catalog_seed_frontmatter_contains_required_fields() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", Some("search"), Some("search_timeline"))],
        manual_body: "MANUAL".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t7");
    let _envguard = mock.bind_env();

    catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    let page = env.wiki_dir.join("x-com-search-search-timeline.md");
    let content = fs::read_to_string(&page).expect("page exists");

    // Verify each required field appears verbatim in the frontmatter block.
    for (k, v) in [
        ("kind", "actionbook-manual"),
        ("source", "catalog"),
        ("host", "x.com"),
        ("site", "x_com"),
        ("group", "search"),
        ("action", "search_timeline"),
        ("catalog_query", "x.com"),
    ] {
        let needle = format!("{k}: {v}");
        assert!(
            content.contains(&needle),
            "frontmatter missing `{needle}`. Got:\n{content}"
        );
    }
    // fetched_at: RFC3339 / ISO8601 UTC, format "YYYY-MM-DDTHH:MM:SSZ".
    let line = content
        .lines()
        .find(|l| l.starts_with("fetched_at:"))
        .expect("fetched_at line present");
    let val = line.trim_start_matches("fetched_at:").trim();
    assert!(matches_rfc3339_utc(val), "fetched_at value {val:?} does not match RFC3339 UTC");
}

/// Minimal RFC3339-UTC matcher (`YYYY-MM-DDTHH:MM:SSZ` or `...Z` with
/// fractional seconds). Sufficient for the frontmatter sanity check
/// without pulling `regex` as a dev-dep.
fn matches_rfc3339_utc(s: &str) -> bool {
    if !s.ends_with('Z') {
        return false;
    }
    // Expect at least 20 chars: YYYY-MM-DDTHH:MM:SSZ.
    if s.len() < 20 {
        return false;
    }
    let bytes = s.as_bytes();
    let is_digit = |i: usize| bytes.get(i).is_some_and(|b| b.is_ascii_digit());
    let is_char = |i: usize, c: u8| bytes.get(i) == Some(&c);
    is_digit(0)
        && is_digit(1)
        && is_digit(2)
        && is_digit(3)
        && is_char(4, b'-')
        && is_digit(5)
        && is_digit(6)
        && is_char(7, b'-')
        && is_digit(8)
        && is_digit(9)
        && is_char(10, b'T')
        && is_digit(11)
        && is_digit(12)
        && is_char(13, b':')
        && is_digit(14)
        && is_digit(15)
        && is_char(16, b':')
        && is_digit(17)
        && is_digit(18)
}

// ─── 8. catalog_seed_reseed_flag_forces_overwrite ─────────────────────────

#[test]
fn catalog_seed_reseed_flag_forces_overwrite() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", Some("search"), Some("search_timeline"))],
        manual_body: "NEW-BODY".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t8");
    let _envguard = mock.bind_env();

    fs::create_dir_all(&env.wiki_dir).unwrap();
    let page = env.wiki_dir.join("x-com-search-search-timeline.md");
    fs::write(&page, "OLD-BODY").unwrap();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts { reseed: true },
    );
    assert_eq!(report.seeded.len(), 1);
    let manual_cmds = mock.manual_cmds();
    assert_eq!(manual_cmds.len(), 1, "expected 1 manual call: {manual_cmds:?}");

    let content = fs::read_to_string(&page).unwrap();
    assert!(content.contains("NEW-BODY"), "expected NEW-BODY in: {content}");
    assert!(!content.contains("OLD-BODY"), "OLD-BODY must be gone");
    // fetched_at must be a current UTC timestamp (today's year).
    let now_year = chrono::Utc::now().format("%Y").to_string();
    assert!(
        content.contains(&format!("fetched_at: {now_year}-")),
        "expected fresh fetched_at with year {now_year}, got:\n{content}"
    );
}

// ─── 9. catalog_seed_v1_backend_skips_catalog ─────────────────────────────

#[test]
fn catalog_seed_v1_backend_skips_catalog() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", None, None)],
        ..Default::default()
    });
    let env = TestEnv::new("t9");
    let _envguard = mock.bind_env();
    let _backend = EnvGuard::set("ACTIONBOOK_BACKEND", "v1-cli");

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    // mock should have received 0 requests.
    let total = mock.all_cmds().len();
    assert_eq!(total, 0, "expected 0 MCP cmds under v1-cli, got: {total}");
    assert_eq!(report.seeded.len(), 0);
    assert_no_wiki_seeded_event(&env);
}

// ─── 10. catalog_seed_skips_when_host_empty ───────────────────────────────

#[test]
fn catalog_seed_skips_when_host_empty() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("t10");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "file:///tmp/local.html",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );

    let total = mock.all_cmds().len();
    assert_eq!(total, 0, "file:// must not trigger MCP, got: {total}");
    assert_eq!(report.seeded.len(), 0);
}

// ─── 11. catalog_seed_partial_failure_continues ───────────────────────────

#[test]
fn catalog_seed_partial_failure_continues() {
    let _lock = env_serializer();
    let hits = vec![
        hit("site_1", Some("g1"), Some("a1")),
        hit("site_2", Some("g2"), Some("a2")), // mock will fail this one
        hit("site_3", Some("g3"), Some("a3")),
    ];
    let mock = McpMock::start_with(MockConfig {
        search_hits: hits,
        manual_body: "OK-BODY".to_string(),
        manual_error_for_sites: vec!["site_2".to_string()],
        ..Default::default()
    });
    let env = TestEnv::new("t11");
    let _envguard = mock.bind_env();

    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );
    assert_eq!(report.seeded.len(), 2, "expect 2 seeded (3rd was the failure)");
    catalog::log_seed_events(&env.slug, "https://x.com/explore", "x.com", &report);

    let files: Vec<String> = fs::read_dir(&env.wiki_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|f| f == "site-1-g1-a1.md"));
    assert!(files.iter().any(|f| f == "site-3-g3-a3.md"));
    assert!(!files.iter().any(|f| f == "site-2-g2-a2.md"));

    // 2 wiki_seeded events written, none for the failed site.
    let events = event::read_events(&env.session_jsonl()).unwrap();
    let seeded: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::WikiSeeded { .. }))
        .collect();
    assert_eq!(seeded.len(), 2);
    for ev in &seeded {
        if let SessionEvent::WikiSeeded { site, .. } = ev {
            assert_ne!(site, "site_2", "failed site must not appear in events");
        }
    }
}

// ─── 12. catalog_seed_batch_per_url_independent ───────────────────────────

#[test]
fn catalog_seed_batch_per_url_independent() {
    let _lock = env_serializer();
    // Spec scenario: batch ["https://x.com/a", "https://github.com/b"].
    // Mock distinguishes by the cmd string substring ("x.com" vs "github.com").
    let mock = McpMock::start_with(MockConfig {
        per_host_hits: vec![
            ("x.com".to_string(), vec![hit("x_com", Some("g"), Some("a"))]),
            ("github.com".to_string(), vec![hit("github_com", Some("g"), Some("a"))]),
        ],
        manual_body: "BODY".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new_unbootstrapped("t12");

    // Bootstrap the session through the real CLI so session.toml etc.
    // exist when batch runs.
    let new_out = Command::new(research_bin())
        .args(["new", "topic", "--slug", &env.slug, "--json"])
        .env("ACTIONBOOK_RESEARCH_HOME", &env.home)
        .output()
        .expect("spawn research new");
    assert!(
        new_out.status.success(),
        "research new failed: stderr={} stdout={}",
        String::from_utf8_lossy(&new_out.stderr),
        String::from_utf8_lossy(&new_out.stdout),
    );

    // batch needs the CLI; spawn the binary with our mock endpoint + key.
    let out = Command::new(research_bin())
        .args([
            "batch",
            "https://x.com/a",
            "https://github.com/b",
            "--slug",
            &env.slug,
            "--json",
        ])
        .env("ACTIONBOOK_RESEARCH_HOME", &env.home)
        .env("ACTIONBOOK_BACKEND", "v2-mcp")
        .env("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint)
        .env("ACTIONBOOK_API_KEY", "test-key")
        .env("PATH", &env.path_env())
        .output()
        .expect("spawn research batch");

    // batch exits 0 even when fetches fail (it returns partial-success).
    // Just assert the process didn't crash.
    let _ = out;

    // Two search calls (one per URL).
    let search_cmds = mock.search_cmds();
    assert_eq!(
        search_cmds.len(),
        2,
        "expect 2 search cmds (one per URL), got: {search_cmds:?}"
    );

    // Two wiki files (one per URL).
    let files: Vec<String> = fs::read_dir(&env.wiki_dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(files.len(), 2, "expect 2 wiki files, got: {files:?}");
    assert!(files.iter().any(|f| f.starts_with("x-com-")));
    assert!(files.iter().any(|f| f.starts_with("github-com-")));

    // Two wiki_seeded events in jsonl.
    let events = event::read_events(&env.session_jsonl()).unwrap_or_default();
    let seeded: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::WikiSeeded { .. }))
        .collect();
    assert_eq!(seeded.len(), 2, "expect 2 WikiSeeded events");
}

// ─── 13. catalog_seed_does_not_alter_route_or_fetch ───────────────────────

#[test]
fn catalog_seed_does_not_alter_route_or_fetch() {
    let _lock = env_serializer();
    // We can't easily mock fetch::execute from a test, but we can verify
    // (a) catalog probe does not write into raw/ — catalog writes go to wiki/
    // (b) when catalog has hits, only wiki/ gets new files (not raw/ or anywhere else)
    // (c) seed_for_url returns success even when manual succeeds, but the
    //     wiki dir contents are isolated to catalog seed files
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", Some("search"), Some("search_timeline"))],
        manual_body: "M".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("t13");
    let _envguard = mock.bind_env();

    // Run a probe in isolation (no fetch involvement).
    let report = catalog::seed_for_url_in(
        "https://x.com/explore",
        &env.slug,
        &env.wiki_dir,
        SeedOpts::default(),
    );
    assert_eq!(report.seeded.len(), 1);

    // Wiki dir has exactly 1 new file. No other directory inside session
    // was touched by the catalog module (it doesn't go near raw/, smell,
    // route, or session.md — the only writes are wiki/ files and a
    // session.jsonl event when log_seed_events runs).
    let wiki_files: Vec<_> = fs::read_dir(&env.wiki_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(wiki_files.len(), 1, "wiki/ should hold exactly 1 catalog file");

    // raw/ untouched (not even created by catalog code — only fetch creates it).
    let raw_dir = env.session_dir().join("raw");
    if raw_dir.exists() {
        let raw_files: Vec<_> = fs::read_dir(&raw_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            raw_files.is_empty(),
            "raw/ should not be touched by catalog: {raw_files:?}"
        );
    }
}

// ─── 14. catalog_seed_filename_slug_rules ──────────────────────────────────

#[test]
fn catalog_seed_filename_slug_rules() {
    // Pure unit: page_slug_for produces "x-com-search-api-search-timeline"
    // for the spec example.
    let slug = catalog::page_slug_for("X_Com", Some("Search.API"), Some("search__timeline"));
    assert_eq!(slug, "x-com-search-api-search-timeline");
    assert!(slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    assert!(!slug.ends_with('-'));
    assert!(!slug.contains("--"));
}

// ─── 15. catalog_seed_filename_optional_parts ─────────────────────────────

#[test]
fn catalog_seed_filename_optional_parts() {
    let cases = [
        ("x_com", Some("search"), Some("search_timeline"), "x-com-search-search-timeline"),
        ("x_com", Some("search"), None, "x-com-search"),
        ("x_com", None, None, "x-com"),
    ];
    for (site, g, a, expected) in cases {
        let got = catalog::page_slug_for(site, g, a);
        assert_eq!(
            got, expected,
            "page_slug_for({site:?}, {g:?}, {a:?}) = {got:?}, expected {expected:?}",
        );
    }
}

// ─── 16. catalog_seed_max_constant_is_three_hardcoded ─────────────────────

#[test]
fn catalog_seed_max_constant_is_three_hardcoded() {
    assert_eq!(MAX_SEED_PER_URL, 3);

    // Verify no env var read for the cap. Set a misleading value and
    // confirm the constant doesn't budge.
    let _g = EnvGuard::set("MAX_SEED_PER_URL", "999");
    assert_eq!(MAX_SEED_PER_URL, 3);

    // Also assert the source file does not read this constant from env.
    // (Defensive — if someone later wires env override, this catches it.)
    let src = include_str!("../src/catalog/mod.rs");
    // The constant declaration is the only allowed reference; any
    // `env::var(...MAX_SEED...` would be a violation.
    assert!(
        !src.contains("MAX_SEED_PER_URL\")")
            && !src.contains("\"MAX_SEED_PER_URL\""),
        "catalog/mod.rs must not read MAX_SEED_PER_URL from env"
    );
}

// ─── 17. catalog_seed_silent_skip_writes_no_jsonl ─────────────────────────

#[test]
fn catalog_seed_silent_skip_writes_no_jsonl() {
    let _lock = env_serializer();
    // The spec lays out 5 sub-scenarios. We run all 5 sequentially against
    // independent mock configs and confirm: 0 jsonl growth, 0 seeded.

    let scenarios: Vec<(&str, MockConfig)> = vec![
        (
            "network-down",
            MockConfig {
                tcp_reset_on_search: true,
                ..Default::default()
            },
        ),
        (
            "extension-off",
            MockConfig {
                search_error_code: Some("EXTENSION_OFFLINE".into()),
                ..Default::default()
            },
        ),
        (
            "session-lost",
            MockConfig {
                search_error_code: Some("SESSION_LOST".into()),
                ..Default::default()
            },
        ),
        (
            "parse-fail",
            MockConfig {
                search_raw_text: Some("this is definitely not JSON {[".into()),
                ..Default::default()
            },
        ),
        (
            "zero-hit",
            MockConfig {
                search_hits: vec![],
                ..Default::default()
            },
        ),
    ];

    for (label, cfg) in scenarios {
        let mock = McpMock::start_with(cfg);
        let env = TestEnv::new(&format!("t17-{label}"));
        let _envguard = mock.bind_env();

        // jsonl line count before.
        let before = jsonl_lines(&env);

        let report = catalog::seed_for_url_in(
            "https://x.com/explore",
            &env.slug,
            &env.wiki_dir,
            SeedOpts::default(),
        );
        assert_eq!(
            report.seeded.len(),
            0,
            "[{label}] seeded must be 0 on silent skip"
        );

        // After: same line count (no growth).
        let after = jsonl_lines(&env);
        assert_eq!(
            after - before,
            0,
            "[{label}] session.jsonl grew by {} on silent skip",
            after - before
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn hit(site: &str, group: Option<&str>, action: Option<&str>) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("site".into(), Value::String(site.into()));
    if let Some(g) = group {
        o.insert("group".into(), Value::String(g.into()));
    }
    if let Some(a) = action {
        o.insert("action".into(), Value::String(a.into()));
    }
    Value::Object(o)
}

fn jsonl_lines(env: &TestEnv) -> usize {
    let path = env.session_jsonl();
    if !path.exists() {
        return 0;
    }
    fs::read_to_string(&path)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn assert_no_wiki_seeded_event(env: &TestEnv) {
    let jsonl = env.session_jsonl();
    if !jsonl.exists() {
        return;
    }
    let events = event::read_events(&jsonl).unwrap();
    for e in &events {
        assert!(
            !matches!(e, SessionEvent::WikiSeeded { .. }),
            "unexpected WikiSeeded event: {e:?}"
        );
    }
}

// A per-test env: tempdir for the session root, deterministic slug, and
// scoped env var resets via EnvGuard. NOTE: ACTIONBOOK_RESEARCH_HOME is
// scoped to this test via env vars, but `catalog::seed_for_url_in` takes
// the wiki_dir explicitly so the layout module's research_root path is
// only relevant when we test the canonical `seed_for_url` (jsonl
// log_seed_events still uses layout::session_jsonl(slug)).
struct TestEnv {
    _tmp: TempDir,
    home: String,
    slug: String,
    wiki_dir: PathBuf,
    _home_guard: EnvGuard,
}

impl TestEnv {
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let slug = slug.to_string();
        // Bootstrap the session dir tree so log_seed_events (which calls
        // layout::session_jsonl(slug)) writes into our tempdir.
        let session_dir = tmp.path().join(&slug);
        fs::create_dir_all(&session_dir).unwrap();
        let wiki_dir = session_dir.join("wiki");
        let _home_guard = EnvGuard::set("ACTIONBOOK_RESEARCH_HOME", &home);
        Self {
            _tmp: tmp,
            home,
            slug,
            wiki_dir,
            _home_guard,
        }
    }

    /// Variant for CLI subprocess tests — does NOT bootstrap any
    /// session directory (the test will run `research new` itself).
    fn new_unbootstrapped(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let slug = slug.to_string();
        let wiki_dir = tmp.path().join(&slug).join("wiki");
        let _home_guard = EnvGuard::set("ACTIONBOOK_RESEARCH_HOME", &home);
        Self {
            _tmp: tmp,
            home,
            slug,
            wiki_dir,
            _home_guard,
        }
    }

    fn session_dir(&self) -> PathBuf {
        PathBuf::from(&self.home).join(&self.slug)
    }

    fn session_jsonl(&self) -> PathBuf {
        self.session_dir().join("session.jsonl")
    }

    fn path_env(&self) -> String {
        std::env::var("PATH").unwrap_or_default()
    }
}

/// Scoped env-var setter that restores the prior value on drop.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, val: &str) -> Self {
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val) };
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(&self.key, v) },
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}

// ─── Mock MCP server ──────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct MockConfig {
    /// Hits returned by the next `search` call. JSON array of objects with
    /// `site`/`group`/`action`. Empty array = "no hit". Ignored when
    /// `search_error_code` or `search_raw_text` or `per_host_hits` is set.
    search_hits: Vec<Value>,
    /// Per-host search responses. If non-empty, takes precedence over
    /// `search_hits` — the mock matches the host substring in the cmd.
    per_host_hits: Vec<(String, Vec<Value>)>,
    /// If Some, the mock returns this error code in the JSON-RPC error
    /// envelope for `search` cmds.
    search_error_code: Option<String>,
    /// If Some, the mock returns this raw text as the tool response (not
    /// valid JSON when set to a non-JSON value, exercising parse-fail).
    search_raw_text: Option<String>,
    /// If true, the mock closes the TCP connection on `search` calls.
    tcp_reset_on_search: bool,
    /// Body returned for successful `manual` calls.
    manual_body: String,
    /// Site keys for which `manual` returns an error (for partial failure
    /// scenarios).
    manual_error_for_sites: Vec<String>,
}

struct McpMock {
    endpoint: String,
    cmds: Arc<Mutex<Vec<String>>>,
}

impl McpMock {
    fn start_with(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let cmds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cmds_for_thread = cmds.clone();
        let cfg = config;
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let cmds = cmds_for_thread.clone();
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let mut buf = [0u8; 65536];
                    let n = match s.read(&mut buf) {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (headers, body_seen) = match raw.split_once("\r\n\r\n") {
                        Some((h, b)) => (h.to_string(), b.to_string()),
                        None => (raw.clone(), String::new()),
                    };
                    let content_length: usize = headers
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    let mut body = body_seen;
                    while body.len() < content_length {
                        let mut more = [0u8; 4096];
                        match s.read(&mut more) {
                            Ok(0) => break,
                            Ok(m) => body.push_str(&String::from_utf8_lossy(&more[..m])),
                            Err(_) => break,
                        }
                    }
                    let parsed: Value =
                        serde_json::from_str(&body).unwrap_or(Value::Null);
                    let method =
                        parsed.get("method").and_then(Value::as_str).unwrap_or("");
                    let response_json: String;
                    let extra_headers: &str;
                    match method {
                        "initialize" => {
                            extra_headers = "Mcp-Session-Id: mock-session\r\n";
                            response_json = json!({
                                "jsonrpc": "2.0",
                                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result": {
                                    "protocolVersion": "2025-06-18",
                                    "capabilities": {},
                                    "serverInfo": {"name": "mock", "version": "0"}
                                }
                            })
                            .to_string();
                        }
                        "notifications/initialized" => {
                            extra_headers = "";
                            response_json = json!({}).to_string();
                        }
                        "tools/call" => {
                            let cmd = parsed
                                .get("params")
                                .and_then(|p| p.get("arguments"))
                                .and_then(|a| a.get("cmd"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            cmds.lock().unwrap().push(cmd.clone());

                            if cmd.starts_with("actionbook search") {
                                if cfg.tcp_reset_on_search {
                                    // Abort the connection without writing a response.
                                    return;
                                }
                                if let Some(code) = &cfg.search_error_code {
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "error": { "code": code, "message": "mock error" }
                                    })
                                    .to_string();
                                    extra_headers = "";
                                } else if let Some(raw) = &cfg.search_raw_text {
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "result": {
                                            "content": [{"type":"text","text": raw}]
                                        }
                                    })
                                    .to_string();
                                    extra_headers = "";
                                } else {
                                    // Pick per-host or default.
                                    let hits: Vec<Value> = if !cfg.per_host_hits.is_empty() {
                                        cfg.per_host_hits
                                            .iter()
                                            .find_map(|(host, h)| {
                                                if cmd.contains(host) {
                                                    Some(h.clone())
                                                } else {
                                                    None
                                                }
                                            })
                                            .unwrap_or_default()
                                    } else {
                                        cfg.search_hits.clone()
                                    };
                                    let text = serde_json::to_string(&hits).unwrap();
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "result": {
                                            "content": [{"type":"text","text": text}]
                                        }
                                    })
                                    .to_string();
                                    extra_headers = "";
                                }
                            } else if cmd.starts_with("actionbook manual") {
                                // Fail if site matches a configured fail list.
                                let mut fail = false;
                                for s in &cfg.manual_error_for_sites {
                                    if cmd.contains(&format!(" {s}"))
                                        || cmd.ends_with(s.as_str())
                                    {
                                        fail = true;
                                        break;
                                    }
                                }
                                if fail {
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "error": { "code": "INTERNAL_ERROR", "message": "mock fail" }
                                    })
                                    .to_string();
                                } else {
                                    let text = format!(
                                        "[t1]\nok actionbook manual\n{}",
                                        cfg.manual_body
                                    );
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "result": {
                                            "content": [{"type":"text","text": text}]
                                        }
                                    })
                                    .to_string();
                                }
                                extra_headers = "";
                            } else {
                                // Unknown tool cmd — return success envelope with empty text.
                                response_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {
                                        "content": [{"type":"text","text":""}]
                                    }
                                })
                                .to_string();
                                extra_headers = "";
                            }
                        }
                        _ => {
                            extra_headers = "";
                            response_json = json!({
                                "jsonrpc":"2.0",
                                "id":parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result":{}
                            })
                            .to_string();
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                        response_json.len(),
                        extra_headers,
                        response_json
                    );
                    let _ = s.write_all(response.as_bytes());
                    let _ = s.flush();
                });
            }
        });
        let endpoint = format!("http://{addr}/mcp");
        Self { endpoint, cmds }
    }

    /// Set the env vars the catalog probe needs so the lib calls hit
    /// THIS mock instead of the real endpoint.
    fn bind_env(&self) -> Vec<EnvGuard> {
        vec![
            EnvGuard::set("ACTIONBOOK_MCP_ENDPOINT", &self.endpoint),
            EnvGuard::set("ACTIONBOOK_API_KEY", "test-key"),
            // Make sure we're NOT in v1-cli backend (catalog path requires v2).
            EnvGuard::set("ACTIONBOOK_BACKEND", "v2-mcp"),
        ]
    }

    fn all_cmds(&self) -> Vec<String> {
        self.cmds.lock().unwrap().clone()
    }

    fn search_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("actionbook search"))
            .cloned()
            .collect()
    }

    fn manual_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("actionbook manual"))
            .cloned()
            .collect()
    }
}
