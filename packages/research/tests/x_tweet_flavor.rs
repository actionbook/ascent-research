//! Integration tests for `x-com-tweet-runcode-flavor.spec.md`.
//!
//! Coverage map (BDD scenario → test name, all named exactly as the spec
//! § 验收标准 requires):
//!
//! 1.  flavor_for_url_x_tweet_detail
//! 2.  flavor_for_url_x_profile
//! 3.  flavor_for_url_x_search
//! 4.  flavor_for_url_twitter_legacy_mirror
//! 5.  flavor_for_url_mobile_x
//! 6.  flavor_for_url_www_x
//! 7.  flavor_for_url_github_is_default
//! 8.  flavor_for_url_hn_is_default
//! 9.  flavor_for_url_malformed_falls_back_default
//! 10. x_tweet_js_contains_article_tweet_selector
//! 11. x_tweet_js_contains_cell_inner_div_selector
//! 12. x_tweet_js_contains_user_name_selector
//! 13. x_tweet_js_joins_articles_and_falls_back_to_body  (was: reads_article_with_body_fallback)
//! 13a. x_tweet_js_uses_query_selector_all_for_thread     (new: thread support)
//! 13b. x_tweet_js_scrolls_to_load_thread                 (new: scroll lazy-load)
//! 13c. x_tweet_js_caps_max_articles                      (new: MAX_ARTICLES cap)
//! 13d. x_tweet_js_breaks_when_no_new_articles            (new: early termination)
//! 14. x_tweet_js_omits_networkidle
//! 15. default_js_still_contains_networkidle
//! 16. build_runcode_cmd_for_url_x_uses_x_tweet_js
//! 17. build_runcode_cmd_for_url_github_uses_default_js
//! 18. tech_preset_has_x_tweet_status_rule
//! 19. tech_preset_has_x_profile_rule
//! 20. tech_preset_has_x_search_live_rule
//! 21. github_issue_route_still_resolves_after_x_rules_added
//!
//! All tests are unit-style — call library helpers directly, no
//! network, no MCP mock needed.

use research::fetch::browser_v2::{
    build_runcode_cmd_for_url, flavor_for_url, runcode_inline_js, runcode_inline_js_x_tweet,
    RuncodeFlavor,
};

// ═══════════════════════════════════════════════════════════════════════════
// 1-9. flavor_for_url URL dispatch
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn flavor_for_url_x_tweet_detail() {
    let url = "https://x.com/yoh2_sdj/status/2055889268883796342";
    assert_eq!(flavor_for_url(url), RuncodeFlavor::XTweet);
}

#[test]
fn flavor_for_url_x_profile() {
    assert_eq!(
        flavor_for_url("https://x.com/yoh2_sdj"),
        RuncodeFlavor::XTweet
    );
}

#[test]
fn flavor_for_url_x_search() {
    assert_eq!(
        flavor_for_url("https://x.com/search?q=bun%20rust&f=live"),
        RuncodeFlavor::XTweet
    );
}

#[test]
fn flavor_for_url_twitter_legacy_mirror() {
    assert_eq!(
        flavor_for_url("https://twitter.com/jarred/status/123"),
        RuncodeFlavor::XTweet
    );
}

#[test]
fn flavor_for_url_mobile_x() {
    assert_eq!(
        flavor_for_url("https://mobile.x.com/foo/status/1"),
        RuncodeFlavor::XTweet
    );
}

#[test]
fn flavor_for_url_www_x() {
    assert_eq!(
        flavor_for_url("https://www.x.com/jarredsumner"),
        RuncodeFlavor::XTweet
    );
}

#[test]
fn flavor_for_url_github_is_default() {
    assert_eq!(
        flavor_for_url("https://github.com/oven-sh/bun/pull/30728"),
        RuncodeFlavor::Default
    );
}

#[test]
fn flavor_for_url_hn_is_default() {
    assert_eq!(
        flavor_for_url("https://news.ycombinator.com/item?id=1"),
        RuncodeFlavor::Default
    );
}

#[test]
fn flavor_for_url_malformed_falls_back_default() {
    // Must not panic; must downgrade to Default.
    assert_eq!(flavor_for_url("not a url"), RuncodeFlavor::Default);
    assert_eq!(flavor_for_url(""), RuncodeFlavor::Default);
    assert_eq!(flavor_for_url("ftp://no/scheme/handler"), RuncodeFlavor::Default);
}

// ═══════════════════════════════════════════════════════════════════════════
// 10-14. XTweet inline JS shape
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn x_tweet_js_contains_article_tweet_selector() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains(r#"article[data-testid="tweet"]"#),
        "XTweet JS must contain the tweet article selector; got: {js}"
    );
}

#[test]
fn x_tweet_js_contains_cell_inner_div_selector() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains(r#"[data-testid="cellInnerDiv"]"#),
        "XTweet JS must contain the cellInnerDiv selector; got: {js}"
    );
}

#[test]
fn x_tweet_js_contains_user_name_selector() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains(r#"[data-testid="UserName"]"#),
        "XTweet JS must contain the UserName selector; got: {js}"
    );
}

// Spec scenario: "XTweet JS join 多个 article.innerText 用 thematic break
// 分隔,fallback 到 body". Renamed from the v0.4.0 single-article variant
// to reflect the thread-aware impl (querySelectorAll + join + body
// fallback when no articles matched).
#[test]
fn x_tweet_js_joins_articles_and_falls_back_to_body() {
    let js = runcode_inline_js_x_tweet();
    // multi-article join path uses a shortened binding (`a.innerText`)
    assert!(
        js.contains("a.innerText") || js.contains("article.innerText"),
        "XTweet JS must read each article's innerText; got: {js}"
    );
    assert!(
        js.contains("document.body.innerText"),
        "XTweet JS must fall back to document.body.innerText when no articles match"
    );
    assert!(
        js.contains("---"),
        "XTweet JS must use markdown thematic break ('---') as article separator; got: {js}"
    );
}

// Spec scenario: "XTweet JS 用 querySelectorAll 抓 thread 多 article"
#[test]
fn x_tweet_js_uses_query_selector_all_for_thread() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("querySelectorAll"),
        "XTweet JS must use querySelectorAll to collect all thread articles; got: {js}"
    );
    assert!(
        js.contains(r#"article[data-testid="tweet"]"#),
        "querySelectorAll must target the tweet article selector; got: {js}"
    );
}

// Spec scenario: "XTweet JS 用增量 scrollBy(防 virtualized 卸载主推)"
#[test]
fn x_tweet_js_scrolls_incrementally() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("scrollBy"),
        "XTweet JS must use incremental scrollBy (not jump-to-bottom); got: {js}"
    );
    assert!(
        js.contains("innerHeight * 0.8"),
        "XTweet JS must scroll by 0.8 × viewport per step (gives early articles time before unmount); got: {js}"
    );
    assert!(
        !js.contains("scrollTo(0, document.body.scrollHeight)"),
        "XTweet JS must NOT jump-to-bottom (loses main tweet to virtualization); got: {js}"
    );
}

// Spec scenario: "XTweet JS snapshot 在滚动循环之前先采集一次(主推必须保留)"
#[test]
fn x_tweet_js_snapshots_before_first_scroll() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("snapshot()"),
        "XTweet JS must define and call snapshot() helper; got: {js}"
    );
    // The first snapshot() call must precede the `for (` scroll loop.
    let first_snap = js.find("snapshot()").unwrap_or(usize::MAX);
    let for_loop = js.find("for (").unwrap_or(usize::MAX);
    assert!(
        first_snap < for_loop,
        "snapshot() must be invoked BEFORE the scroll loop so the main tweet \
         (first-screen-visible, unmounted after scroll) is captured; got: snapshot@{} vs for@{} in {}",
        first_snap, for_loop, js
    );
}

// Spec scenario: "XTweet JS 用 Map + tweetId 跨 snapshot 去重"
#[test]
fn x_tweet_js_uses_tweet_id_map_for_dedup() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("new Map()"),
        "XTweet JS must use a Map for cross-snapshot dedup; got: {js}"
    );
    assert!(
        js.contains("/status/"),
        "XTweet JS must extract tweetId from /USER/status/<id> link; got: {js}"
    );
    assert!(
        js.contains("seen.has(id)"),
        "XTweet JS must guard against re-adding already-seen tweetIds; got: {js}"
    );
}

// Spec scenario: "XTweet JS fallback tweetId 防止无 link 的 article 漏掉"
#[test]
fn x_tweet_js_uses_idx_fallback_for_articles_without_link() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("idx-"),
        "XTweet JS must provide fallback tweetId for link-less articles; got: {js}"
    );
    assert!(
        js.contains("seen.size"),
        "XTweet JS must use Map size to disambiguate fallback ids; got: {js}"
    );
}

// Spec scenario: "XTweet JS 设 thread article cap 防止无限滚动"
#[test]
fn x_tweet_js_caps_max_articles() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("MAX_ARTICLES"),
        "XTweet JS must define MAX_ARTICLES cap as a named constant; got: {js}"
    );
    assert!(
        js.contains("slice"),
        "XTweet JS must slice(0, MAX_ARTICLES) as belt-and-braces cap; got: {js}"
    );
}

// Spec scenario: "XTweet JS 无 snapshot 进展即停"
#[test]
fn x_tweet_js_breaks_when_no_new_articles() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("before"),
        "XTweet JS must track 'before' Map size to detect zero-progress; got: {js}"
    );
    assert!(
        js.contains("seen.size === before") || js.contains("seen.size == before"),
        "XTweet JS must break when no new articles appeared after scroll; got: {js}"
    );
}

// Spec scenario: "XTweet JS 抽取推文附图(pbs.twimg.com/media)"
#[test]
fn x_tweet_js_extracts_pbs_twimg_media_images() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("pbs.twimg.com/media"),
        "XTweet JS must whitelist pbs.twimg.com/media (tweet attachment images); got: {js}"
    );
    // querySelectorAll input — either single-quoted or double-quoted form
    assert!(
        js.contains("querySelectorAll('img')") || js.contains(r#"querySelectorAll("img")"#),
        "XTweet JS must enumerate img elements via querySelectorAll; got: {js}"
    );
}

// Spec scenario: "XTweet JS 抽取视频 poster(pbs.twimg.com/tweet_video_thumb)"
#[test]
fn x_tweet_js_extracts_video_poster() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("pbs.twimg.com/tweet_video_thumb"),
        "XTweet JS must whitelist pbs.twimg.com/tweet_video_thumb (video posters); got: {js}"
    );
    assert!(
        js.contains("querySelectorAll('video')") || js.contains(r#"querySelectorAll("video")"#),
        "XTweet JS must enumerate video elements; got: {js}"
    );
    assert!(
        js.contains("v.poster"),
        "XTweet JS must read video.poster as the first-frame thumbnail; got: {js}"
    );
}

// Spec scenario: "XTweet JS 抽取链接卡片图(pbs.twimg.com/card_img)"
#[test]
fn x_tweet_js_extracts_card_image() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("pbs.twimg.com/card_img"),
        "XTweet JS must whitelist pbs.twimg.com/card_img (link preview images); got: {js}"
    );
}

// Spec scenario: "XTweet JS 输出 markdown image 语法"
#[test]
fn x_tweet_js_emits_markdown_image_syntax() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        js.contains("![]("),
        "XTweet JS must emit markdown image syntax `![](url)` for each captured media URL; got: {js}"
    );
    assert!(
        !js.contains("<img src"),
        "XTweet JS must NOT emit HTML <img src=...> (markdown only); got: {js}"
    );
}

// Spec scenario: "XTweet JS 不抓 avatar / emoji 图床(防噪声)"
#[test]
fn x_tweet_js_does_not_match_avatars_or_emoji() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        !js.contains("profile_images"),
        "XTweet JS must NOT whitelist profile_images/ (avatar noise); got: {js}"
    );
    assert!(
        !js.contains("abs-0.twimg.com/emoji"),
        "XTweet JS must NOT whitelist abs-0.twimg.com/emoji (twemoji noise); got: {js}"
    );
}

#[test]
fn x_tweet_js_omits_networkidle() {
    let js = runcode_inline_js_x_tweet();
    assert!(
        !js.contains("networkidle"),
        "XTweet JS must NOT wait on networkidle — X never reaches idle; got: {js}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. Default JS regression guard
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn default_js_still_contains_networkidle() {
    let js = runcode_inline_js();
    assert!(
        js.contains("networkidle"),
        "Default JS must still wait on networkidle — regression of v0.4.0 behavior; got: {js}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 16-17. build_runcode_cmd_for_url integration
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn build_runcode_cmd_for_url_x_uses_x_tweet_js() {
    let url = "https://x.com/yoh2_sdj/status/2055889268883796342";
    let cmd = build_runcode_cmd_for_url(url, "h", 90_000, None, None);
    assert!(
        cmd.contains(r#"article[data-testid="tweet"]"#),
        "x.com URL must inject the XTweet selector; got: {cmd}"
    );
    assert!(
        !cmd.contains("networkidle"),
        "x.com URL must NOT carry Default flavor's networkidle wait; got: {cmd}"
    );
}

#[test]
fn build_runcode_cmd_for_url_github_uses_default_js() {
    let url = "https://github.com/oven-sh/bun/pull/30728";
    let cmd = build_runcode_cmd_for_url(url, "h", 90_000, None, None);
    assert!(
        cmd.contains("networkidle"),
        "github.com URL must carry Default flavor's networkidle wait; got: {cmd}"
    );
    assert!(
        !cmd.contains(r#"article[data-testid="tweet"]"#),
        "github.com URL must NOT inject XTweet selector; got: {cmd}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 18-21. Preset rule presence + route regression
// ═══════════════════════════════════════════════════════════════════════════

use research::route::{self, Classification, Executor};
use research::route::rules::{CompiledPreset, CompiledRule, PathMatcher, RuleBackend, SegmentPattern};

fn load_tech() -> CompiledPreset {
    route::load_preset(Some("tech"), None).expect("tech preset must load")
}

fn rule_by_kind<'a>(preset: &'a CompiledPreset, kind: &str) -> &'a CompiledRule {
    preset
        .rules
        .iter()
        .find(|r| r.kind == kind)
        .unwrap_or_else(|| panic!("tech preset must contain a rule with kind = {kind}"))
}

// Assert the rule is single-backend with `executor = "browser"`.
fn assert_browser_single(rule: &CompiledRule) {
    match &rule.backend {
        RuleBackend::Single { executor, .. } => {
            assert_eq!(
                executor, "browser",
                "rule `{}` executor must be \"browser\", got {executor}",
                rule.kind
            );
        }
        RuleBackend::Composite(_) => panic!(
            "rule `{}` must be single-backend, got composite",
            rule.kind
        ),
    }
}

#[test]
fn tech_preset_has_x_tweet_status_rule() {
    let preset = load_tech();
    let rule = rule_by_kind(&preset, "x-tweet-status");
    assert_eq!(rule.host, "x.com");
    assert_browser_single(rule);
    match &rule.path_matcher {
        PathMatcher::Segments(segs) => {
            assert_eq!(segs.len(), 3, "x-tweet-status must have 3 path segments");
            // Pattern: {user}/status/{id}
            assert!(
                matches!(&segs[1], SegmentPattern::Literal(s) if s == "status"),
                "second segment must be the literal \"status\""
            );
        }
        other => panic!("x-tweet-status must use Segments matcher, got {other:?}"),
    }
}

#[test]
fn tech_preset_has_x_profile_rule() {
    let preset = load_tech();
    let rule = rule_by_kind(&preset, "x-profile");
    assert_eq!(rule.host, "x.com");
    assert_browser_single(rule);
    match &rule.path_matcher {
        PathMatcher::Segments(segs) => {
            assert_eq!(segs.len(), 1, "x-profile must have exactly 1 path segment");
            assert!(
                matches!(&segs[0], SegmentPattern::Capture(_)),
                "x-profile single segment must be a Capture, got {:?}",
                segs[0]
            );
        }
        other => panic!("x-profile must use Segments matcher, got {other:?}"),
    }
}

#[test]
fn tech_preset_has_x_search_live_rule() {
    let preset = load_tech();
    let rule = rule_by_kind(&preset, "x-search-live");
    assert_eq!(rule.host, "x.com");
    assert_browser_single(rule);
    match &rule.path_matcher {
        PathMatcher::Segments(segs) => {
            assert_eq!(segs.len(), 1, "x-search-live must have 1 path segment");
            match &segs[0] {
                SegmentPattern::Literal(s) => {
                    assert_eq!(s, "search");
                }
                other => panic!("x-search-live segment must be Literal(\"search\"), got {other:?}"),
            }
        }
        other => panic!("x-search-live must use Segments matcher, got {other:?}"),
    }
}

#[test]
fn github_issue_route_still_resolves_after_x_rules_added() {
    let preset = load_tech();
    let url = "https://github.com/oven-sh/bun/issues/30719";
    let classification =
        route::classify(&preset, url, false).expect("github issue URL must classify");
    let route_obj = match &classification {
        Classification::Matched(r) | Classification::Fallback(r) | Classification::Forced(r) => r,
    };
    assert_eq!(route_obj.kind, "github-issue");
    assert!(
        matches!(route_obj.executor, Executor::Postagent),
        "github-issue executor must be Postagent, got {:?}",
        route_obj.executor
    );
}
