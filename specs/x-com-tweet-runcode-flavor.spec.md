spec: task
name: "x-com-tweet-runcode-flavor"
inherits: project
tags: [research-cli, fetch, browser, actionbook-v2, runcode, x-com]
estimate: 0.5d
depends: [actionbook-v2-mcp-backend]
---

## 意图

V2 默认 runcode(`fetch/browser_v2.rs::runcode_inline_js`)对 x.com 推文
详情页失效:返回 ≤200 bytes 的左侧导航 chrome,推文 `<article>` 主体没
有被捕获。根因是 X 把 tweet body 放在独立的 GraphQL `TweetDetail` 请求
里 lazy-load,触发顺序是 `domcontentloaded → networkidle → GraphQL 请求
回来 → React 写入 <article>`,而默认 runcode 在 networkidle 之后立刻读
`document.body.innerText`,只拿到左 nav。`body.innerText.length > 100`
这条 hydration 探测也没用 —— nav chrome 本身已 > 100 字符。

V2 server 没有 `--wait-selector` flag,等待逻辑必须落在 inline JS 里。
本 spec 引入 per-host **runcode flavor 分发**:URL 命中 x.com / twitter.com
/ mobile.x.com → 用 `XTweet` flavor(等 `article[data-testid="tweet"]`
等多 selector 出现);其它 URL → 保持现 `Default` flavor。

历史脉络:`actionbook-v2-mcp-backend.spec.md` 在"排除范围"提到"per-host
特化 runcode 等待"留到 future spec,本 spec 即兑现该项 + 由 2026-05-17
的 yoh2_sdj tweet 调研(session `bun-rust-rewrite` 06.4 段记录的 V2 抓
推空回返事件)直接触发。

## 已定决策

### Flavor 枚举

```rust
pub enum RuncodeFlavor {
    Default,    // 现有 runcode_inline_js,改名前等价行为
    XTweet,     // 新增 — x.com / twitter.com 系列
}
```

仅两个变体。XTweet 一个 flavor 覆盖 tweet detail / profile / search-live
三种 x.com 页型,因为它们都用同一组 `data-testid` marker(`article[data-testid="tweet"]`、
`[data-testid="cellInnerDiv"]`、`[data-testid="UserName"]`)且差别只在哪
一个会先出现。多 selector OR 即可,不需要为页型分别建子 flavor。

### URL 分发函数

```rust
pub fn flavor_for_url(url: &str) -> RuncodeFlavor
```

匹配规则(host 字符串完全相等比较,不做 wildcard):

| host 命中 | 返回 |
|---|---|
| `x.com` / `www.x.com` / `mobile.x.com` | `XTweet` |
| `twitter.com` / `www.twitter.com` / `mobile.twitter.com` | `XTweet` |
| 其它 host | `Default` |
| URL 无法解析(`url::Url::parse` 错误) | `Default` |

不查 path,不查 query string。`x.com/foo/status/...` 与 `x.com/search?...`
与 `x.com/foo` 共用 XTweet flavor,因为多 selector 已覆盖。

### XTweet inline JS

```javascript
async (page) => {
  try {
    await page.waitForLoadState("domcontentloaded", { timeout: 8000 });
  } catch (_e) {}
  // 不等 networkidle — X 永远不到 idle(后台 polling + ad tracker)
  try {
    await page.waitForSelector(
      'article[data-testid="tweet"], [data-testid="cellInnerDiv"], [data-testid="UserName"]',
      { timeout: 15000 }
    );
  } catch (_e) {}

  // Thread lazy-load + virtualization 防丢:X 的 tweet detail 页面用了
  // virtualized list(react-virtual / windowing),滚出 viewport 的
  // article 会被从 DOM **卸载**。早期实现是"滚到底再 querySelectorAll
  // 一次",**主推必丢**:它在滚动起点是首屏可见的,滚到底之后 React
  // 把它 unmount 了。
  //
  // 正确做法:**snapshot 跨 scroll 持续收集 + 按 tweetId 去重**。每次
  // scroll 前后都 snapshot 一次 DOM 当前可见的 article,把每条按
  // 从 `<a href="/USER/status/<id>">` 链接抽出的 tweetId 存进
  // `Map<id, rendered>`。同一 tweetId 已存的不重写(防多次 snapshot
  // 内容微小漂移)。最后输出 Map 的 values。
  //
  // scrollBy(0, innerHeight * 0.8)(增量滚 80% viewport,而非
  // scrollTo(scrollHeight) 一步到底)→ 让 X 边滚边 hydrate 新一批,
  // **同时尽量让早期 article 还留在 DOM**,降低虚拟化卸载窗口。
  const MAX_SCROLLS = 8;       // 8 × 1.2s = 最长 9.6s 滚动预算
  const MAX_ARTICLES = 25;     // 主推 + 最多 24 thread/回复;长度
                               // 上限保证 run-code envelope 不爆
  const seen = new Map();      // tweetId → rendered article text+media

  const snapshot = () => {
    document.querySelectorAll('article[data-testid="tweet"]').forEach(a => {
      const link = a.querySelector('a[href*="/status/"]');
      const m = link ? link.getAttribute('href').match(/\/status\/(\d+)/) : null;
      const id = m ? m[1] : ('idx-' + seen.size);  // fallback 防止挂 url 的丢
      if (seen.has(id)) return;
      // ... 抽 article.innerText + 媒体 + render markdown
      const rendered = renderArticle(a);
      seen.set(id, rendered);
    });
  };

  snapshot();   // 0. 滚前先采集(主推必须在这里被捕获)
  for (let s = 0; s < MAX_SCROLLS; s++) {
    if (seen.size >= MAX_ARTICLES) break;
    const before = seen.size;
    window.scrollBy(0, window.innerHeight * 0.8);   // 增量滚
    await new Promise(r => setTimeout(r, 1200));
    snapshot();
    if (seen.size === before) break;                // 无新增即停
  }
  await new Promise(r => setTimeout(r, 500));       // 最后 hydration grace
  snapshot();                                        // 最终再补一次

  const articles = Array.from(seen.values()).slice(0, MAX_ARTICLES);

  // 媒体抓取:每个 article 单独抽 img/video URL,附加在 innerText 后面,
  // 用 markdown image 语法 ![](url) —— 报告渲染时浏览器直接命中 X CDN
  // (pbs.twimg.com),用户在 HTML 报告里**点开就能看到原图**;raw md
  // 文件里也是合法 markdown,Obsidian / VS Code preview 渲染。
  const render = (a) => {
    const txt = a.innerText;
    // 只留信号性图床(tweet 附图 / 视频 poster / 链接卡片图);
    // 过滤掉 avatar(profile_images)与 emoji(abs-0.twimg.com/emoji)
    // 这俩噪声 —— 否则每条 reply 都带 4-6 张头像,噪声 > 信号。
    const imgs = Array.from(a.querySelectorAll('img'))
      .map(i => i.src)
      .filter(s =>
        s.includes('pbs.twimg.com/media') ||
        s.includes('pbs.twimg.com/tweet_video_thumb') ||
        s.includes('pbs.twimg.com/card_img')
      );
    const vids = Array.from(a.querySelectorAll('video'))
      .map(v => v.poster || v.src)
      .filter(Boolean);
    const media = imgs.concat(vids)
      .map(u => '![](' + u + ')')
      .join('\n');
    return media ? (txt + '\n\n' + media) : txt;
  };

  const text = articles.length > 0
    ? articles.map(render).join('\n\n---\n\n')
    : document.body.innerText;
  return { url: page.url(), title: await page.title(), text };
}
```

差异点(对比 Default):

- **去掉** `waitForLoadState("networkidle", { timeout: 3000 })` —— X 永
  远不 idle,等了也是白等 3 秒。
- **去掉** `body.innerText.length > 100` 那 20 × 250ms 的 hydration 轮
  询 —— 用 `waitForSelector` 替代,等到 article 元素出现立刻进下一步,
  不消耗剩余预算。
- **加** 500ms `setTimeout` —— selector 命中只代表元素挂上 DOM,文本节
  点可能还在 React 下一帧写,加 grace 期保险。
- **加** thread lazy-load 滚动循环 —— `querySelector` 单 article 不够
  抓 thread,改 `querySelectorAll` + scroll-to-bottom 触发 X GraphQL
  拉新一批 articles,直到 article 数量稳定或达到 MAX_ARTICLES = 25 上
  限(单条主推 + 长 thread + 回复 25 条以内都覆盖)。
- **加** 媒体(img + video poster)抽取 —— innerText 不返回 `<img>`
  / `<video>`,但研究场景"图里的截图 / 图表 / meme"经常携带关键信号。
  per-article 抽 img.src + video.poster,filter 到 pbs.twimg.com 信号
  路径,以 markdown `![](url)` 附加。
- **read** 多 article 用 `'\n\n---\n\n'` 分隔后 join(`---` 是 markdown
  thematic break,下游 markdown 渲染天然分隔);没有 article 时
  fallback `body.innerText`(命中 selector 超时时仍能拿到登录墙或
  "post deleted" 提示)。

### 图床 URL 过滤白名单

| URL 前缀 | 内容 | 是否保留 | 理由 |
|---------|------|---------|------|
| `pbs.twimg.com/media/<id>?format=jpg&name=*` | 推文附图(JPEG / PNG / GIF 静帧) | ✅ 保留 | 研究核心信号(代码截图 / 图表 / meme) |
| `pbs.twimg.com/tweet_video_thumb/<id>` | 视频 poster(首帧缩略图) | ✅ 保留 | 视频载体的视觉信号 |
| `pbs.twimg.com/card_img/<id>` | 链接卡片图(URL preview) | ✅ 保留 | 文章 / 仓库 / PR 的封面 |
| `pbs.twimg.com/profile_images/<id>` | 用户头像 | ❌ 过滤 | 每 reply 4-6 个,噪声 > 信号 |
| `abs-0.twimg.com/emoji/<unicode>.png` | Twitter Emoji(twemoji)PNG | ❌ 过滤 | innerText 里已经有 Unicode 字符 |
| `pbs.twimg.com/ext_tw_video_thumb/*` | 外链视频缩略图 | ⚠ 暂不抓 | 用得少;未来按需补 |
| `abs.twimg.com/icons/<id>` | UI 图标 | ❌ 过滤 | 噪声 |

`includes(...)` 三选一覆盖 95% 的有信号图;白名单短、好审计、好维护。

### 输出格式选择 `![](url)`(markdown image)

| 候选 | 渲染表现 | smell test 兼容性 | 选 |
|-----|---------|-----------------|---|
| `![](url)` markdown | HTML 报告 → `<img src>` 直接渲染;raw md 在 Obsidian / VS Code preview 渲染 | 计入 `text.length`,加分 | ✅ |
| `[image: url]` 纯文本 | 不渲染;用户得手动 copy 链接 | 同上 | ❌ |
| `<img src="url">` HTML | 在 markdown 里 inline HTML 不被所有 renderer 渲染 | 同上 | ❌ |
| 下载图 + base64 inline | 报告 100KB → 5MB,smell 计 inflated | 不可控 | ❌ |

选 markdown 因为:**渲染面**(rich-html 报告 + Obsidian)免费支持,**离线**
影响小(图临时挂掉时还能看 URL),**字节代价**只算一个 URL ≈ 60 字节。

### 不下载图二进制 / 不 OCR

本 spec **只抓 URL**,不下载图,不调 OCR。理由:

- **下载图二进制** = 加 HTTP client 调用 + 处理 X CDN 鉴权失败 case + 文
  件存到 `<session>/raw/<n>-images/` + 路径修正 markdown 链接 → 工程
  量 5-10x,对"研究分析"价值不超过"链接能点开"。**留到 future spec**。
- **OCR** = 加 tesseract dep(模型 15+ MB)+ libleptonica 平台编译 +
  OCR 噪声后处理 + 性能 +1-3s/图 → 同样留到 future spec,可在 URL 抓
  取之后无缝叠加(URL 已是 OCR pipeline 的输入)。

时间预算(worst case):
DOMContentLoaded 8s + selector wait 15s + scroll loop 7.2s
+ 500ms grace = **~30.7s**。注意 caller 默认 `--timeout 90000` 给 V2
server 的 inner budget 是 85s,远大于 worst case,留有 ~54s 余量给慢
SPA(profile / search-live 都比 detail 慢)。

### Thread cap 选 25 的依据

| 类别 | 典型上限 | 25 是否够 |
|------|---------|----------|
| 单条主推(detail page first load) | 1 | 够 |
| 同作者 thread(知名作者长帖) | 5-15 | 够 |
| 主推下 top-level replies(X "Replies" 区) | 5-30 | 够大多数 |
| 嵌套深层 reply | 30+ | **不够,见排除范围** |
| 病毒推文 1000+ 楼回复 | 1000+ | **不够,见排除范围** |

25 覆盖典型 thread / 单层回复;超大 reply 树需要更复杂的 scroll +
"Show more replies" 按钮点击,留到未来 spec。

### `build_runcode_cmd` 扩展

在现有 `pub fn build_runcode_cmd(handle, caller_timeout_ms, frame_id, run_code_args) -> String`
之外**新增** wrapper:

```rust
pub fn build_runcode_cmd_for_url(
    url: &str,
    handle: &str,
    caller_timeout_ms: u64,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> String
```

`for_url` 内部 = `flavor_for_url(url)` + 已查表的 inline JS + 现有的
`handle/timeout/frame_id/args` 拼装。`build_runcode_cmd` 旧签名保留,其
含义改为"显式 Default flavor 路径",直接调用旧体即可。这让 v0.4.0 的
`runcode_flags.rs` 测试(全部针对 Default flavor 的 cmd 字符串组成)零修
改通过。

`fetch/browser_v2.rs::run` 当前调用 `build_runcode_cmd` 的位置改为
`build_runcode_cmd_for_url(url, …)`,URL 已经在该函数签名里有。

### Preset 显式 rule

`presets/tech.toml` 加 3 条 rule(顺序在 fallback 之前):

```toml
[[rule]]
kind = "x-tweet-status"
host = "x.com"
path_segments = ["{user}", "status", "{id}"]
executor = "browser"
template = '''actionbook browser new-tab "{url}" --session <s> --tab <t> && actionbook browser wait network-idle --session <s> --tab <t> && actionbook browser text --session <s> --tab <t>'''

[[rule]]
kind = "x-profile"
host = "x.com"
path_segments = ["{user}"]
executor = "browser"
template = '''actionbook browser new-tab "{url}" --session <s> --tab <t> && actionbook browser wait network-idle --session <s> --tab <t> && actionbook browser text --session <s> --tab <t>'''

[[rule]]
kind = "x-search-live"
host = "x.com"
path_segments = ["search"]
executor = "browser"
template = '''actionbook browser new-tab "{url}" --session <s> --tab <t> && actionbook browser wait network-idle --session <s> --tab <t> && actionbook browser text --session <s> --tab <t>'''
```

template 字段三条 rule 完全一致(就是默认 browser template),作用是:

1. **V1 fallback 仍能跑** —— V1 不执行 inline JS,template 字面 shell-out
   到 `actionbook` v1 CLI。V1 不在本 spec 优化范围,只需保证不破。
2. **路由意图显式** —— `route` 命令 dry-run 时显示 `kind = "x-tweet-status"`
   而非 `"browser-fallback"`,帮 debug 与 audit。
3. **未来加 query_param 约束** —— 后续可在 `x-search-live` 加 `query_param = { f = "live" }`,本 spec 不做。

不需要在 rule 里加新字段(`runcode_variant`、`runcode_js` 之类)—— V2 路
径完全靠 URL host 分发,不依赖 rule kind 流到 browser_v2。

### 不动什么

- Default flavor JS 字符串(`runcode_inline_js`)不修改;v0.4.0 既有
  `runcode_flags.rs` 测试 100% 兼容。
- V1 CLI 路径(`fetch/browser.rs::run_v1_impl`)不修改 —— 它 shell-out
  到 actionbook v1 CLI,本来就用 v1 CLI 自己的 selector wait。
- 路由解析(`route::mod.rs`)签名不变 —— ResolvedRoute 仍只携带
  `template + executor`,新加的 rule kinds 透明走相同 ResolvedRoute 路。
- `fetch::execute` 签名不变。
- `commands/add.rs` / `batch.rs` 不变 —— 它们都是把 URL 透传给
  `fetch::execute` → `browser::run` → `browser_v2::run`,新分发在最末尾
  `browser_v2` 内部完成。

### 风险与缓解

- **风险**:X 改 `data-testid` 命名。
  **缓解**:multi-selector 三个不同形态(tweet / cellInnerDiv / UserName)
  并列,X 一次重命名只可能 break 其一,article fallback 到 body.innerText
  保证返回非空(降级为 chrome,smell test 自然拒)。再加 6 条 BDD 把
  selector 子串 hardcode,防回归静默漂移。
- **风险**:15s selector 等不到(deleted tweet / login wall / network
  pathological)。
  **缓解**:`try { waitForSelector } catch (_) {}` 吞超时,继续读
  `body.innerText`,smell test 在下游(too_short 或 wrong_url)处理。
- **风险**:把 x.com-specific 知识硬编码进通用 V2 backend。
  **缓解**:`flavor_for_url` ~10 行单函数,JS 是一个 `&'static str`,新增
  flavor(Reddit / HN-thread / LinkedIn)follow 同 pattern,不会演变成
  巨型 match。host 列表 hardcode 5 项(包含 mobile 与 www 子域)边际成
  本极低。
- **风险**:用户把 `ACTIONBOOK_BACKEND=v1-cli` 切回 V1,本 spec 修复无
  效。
  **缓解**:文档化(skill 与 README 提一句"x.com 抓取增强仅 v2-mcp 生
  效");V1 仍 fallback 到 v1 actionbook CLI 的 selector wait,**比当前
  情况更好**,不破。

## 边界

### 允许修改

- `packages/research/src/fetch/browser_v2.rs` —— 新增 `RuncodeFlavor`、
  `flavor_for_url`、`runcode_inline_js_x_tweet`、`build_runcode_cmd_for_url`;
  `run()` 改成调用 `build_runcode_cmd_for_url`。
- `packages/research/presets/tech.toml` —— 新增 3 条 `[[rule]]`。
- `packages/research/tests/runcode_flags.rs` —— 新增本 spec BDD(其它现
  有测试不动)。
- `skills/ascent-research/SKILL.md` —— 在 V2 Pitfalls 段加一句"x.com
  hydration 已由 XTweet flavor 处理,不再需要 oembed 备路"。

### 禁止做

- 不引入 `--wait-selector` flag 到 V2 server(那是 actionbook-cloud 的
  事)。
- 不引入 TOML 字段(`runcode_variant`、`runcode_js`)—— JS 留在 Rust。
- 不改 Default flavor JS(零回归)。
- 不修改 V1 path。
- 不为 tweet / profile / search 拆 sub-flavor(multi-selector 已覆盖)。
- 不添加 LinkedIn / Reddit / Facebook flavor(未来 spec)。
- 不实现 oembed 自动 fallback(`composite-source-fetch.spec.md` 范畴)。

## 验收标准

测试包:`packages/research/tests/runcode_flags.rs`(扩展,unit 注明在文)。

场景: x.com tweet-detail URL 命中 XTweet flavor
  测试: flavor_for_url_x_tweet_detail
  假设 URL = "https://x.com/yoh2_sdj/status/2055889268883796342"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: x.com profile URL 命中 XTweet flavor
  测试: flavor_for_url_x_profile
  假设 URL = "https://x.com/yoh2_sdj"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: x.com search-live URL 命中 XTweet flavor
  测试: flavor_for_url_x_search
  假设 URL = "https://x.com/search?q=bun%20rust&f=live"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: twitter.com legacy 镜像命中 XTweet flavor
  测试: flavor_for_url_twitter_legacy_mirror
  假设 URL = "https://twitter.com/jarred/status/123"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: mobile.x.com 子域命中 XTweet flavor
  测试: flavor_for_url_mobile_x
  假设 URL = "https://mobile.x.com/foo/status/1"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: www.x.com 子域命中 XTweet flavor
  测试: flavor_for_url_www_x
  假设 URL = "https://www.x.com/jarredsumner"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::XTweet

场景: github.com URL 不命中 XTweet 走 Default
  测试: flavor_for_url_github_is_default
  假设 URL = "https://github.com/oven-sh/bun/pull/30728"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::Default

场景: HN URL 不命中 XTweet 走 Default
  测试: flavor_for_url_hn_is_default
  假设 URL = "https://news.ycombinator.com/item?id=1"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::Default

场景: 无效 URL 走 Default(不 panic)
  测试: flavor_for_url_malformed_falls_back_default
  假设 URL = "not a url"
  当 调用 flavor_for_url(url)
  那么 返回 RuncodeFlavor::Default
  并且 函数不 panic

场景: XTweet JS 含 article-tweet selector 子串
  测试: x_tweet_js_contains_article_tweet_selector
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 `article[data-testid="tweet"]`

场景: XTweet JS 含 cellInnerDiv selector 子串
  测试: x_tweet_js_contains_cell_inner_div_selector
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 `[data-testid="cellInnerDiv"]`

场景: XTweet JS 含 UserName selector 子串
  测试: x_tweet_js_contains_user_name_selector
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 `[data-testid="UserName"]`

场景: XTweet JS join 多个 article.innerText 用 thematic break 分隔,fallback 到 body
  测试: x_tweet_js_joins_articles_and_falls_back_to_body
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "article.innerText" 或 "a.innerText"(多 article join 路径)
  并且 含子串 "document.body.innerText"(超时 fallback 路径)
  并且 含子串 "---"(thematic break 分隔符,markdown 渲染自然成段)

场景: XTweet JS 用 querySelectorAll 抓 thread 多 article
  测试: x_tweet_js_uses_query_selector_all_for_thread
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "querySelectorAll"
  并且 含子串 `article[data-testid="tweet"]`(配合 querySelectorAll)

场景: XTweet JS 用增量 scrollBy(防 virtualized 卸载主推)
  测试: x_tweet_js_scrolls_incrementally
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "scrollBy"(增量滚)
  并且 含子串 "innerHeight * 0.8"(0.8 viewport step)
  并且 不含子串 "scrollTo(0, document.body.scrollHeight)"(禁止 jump-to-bottom)

场景: XTweet JS snapshot 在滚动循环之前先采集一次(主推必须保留)
  测试: x_tweet_js_snapshots_before_first_scroll
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "snapshot()"
  并且 snapshot() 第一次调用在 for 循环之前(出现位置 < "for (")

场景: XTweet JS 用 Map + tweetId 跨 snapshot 去重
  测试: x_tweet_js_uses_tweet_id_map_for_dedup
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "new Map()"
  并且 含子串 "/status/"(从 article 内链抽 tweetId 的依据)
  并且 含子串 "seen.has(id)"(已抓过的去重保护)

场景: XTweet JS fallback tweetId 防止无 link 的 article 漏掉
  测试: x_tweet_js_uses_idx_fallback_for_articles_without_link
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "idx-"(无 status link 时的 fallback id)
  并且 含子串 "seen.size"(用 Map 当前大小做 fallback 后缀)

场景: XTweet JS 设 thread article cap 防止无限滚动
  测试: x_tweet_js_caps_max_articles
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "MAX_ARTICLES"(常量名出现 = cap 在生效)
  并且 含子串 "slice"(slice(0, MAX_ARTICLES) 双保险)

场景: XTweet JS 滚动到底无新内容立即跳出循环
  测试: x_tweet_js_breaks_when_no_new_articles
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "prevCount"(用前次 count 比较)
  并且 含子串 "break"(到底跳出循环)

场景: XTweet JS 抽取推文附图(pbs.twimg.com/media)
  测试: x_tweet_js_extracts_pbs_twimg_media_images
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "pbs.twimg.com/media"(白名单第一项)
  并且 含子串 "querySelectorAll('img')" 或 "querySelectorAll(\"img\")"(img 抽取入口)

场景: XTweet JS 抽取视频 poster(pbs.twimg.com/tweet_video_thumb)
  测试: x_tweet_js_extracts_video_poster
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "pbs.twimg.com/tweet_video_thumb"(视频 poster 白名单)
  并且 含子串 "querySelectorAll('video')" 或 "querySelectorAll(\"video\")"
  并且 含子串 "v.poster"(读取 video.poster 属性)

场景: XTweet JS 抽取链接卡片图(pbs.twimg.com/card_img)
  测试: x_tweet_js_extracts_card_image
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "pbs.twimg.com/card_img"(链接卡片图白名单第三项)

场景: XTweet JS 输出 markdown image 语法
  测试: x_tweet_js_emits_markdown_image_syntax
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 含子串 "![]("(markdown 图片打开括号)
  并且 不含子串 "<img src"(确认未走 HTML 路径)

场景: XTweet JS 不抓 avatar / emoji 图床(防噪声)
  测试: x_tweet_js_does_not_match_avatars_or_emoji
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 不含子串 "profile_images"(avatar 不在白名单)
  并且 不含子串 "abs-0.twimg.com/emoji"(twemoji 不在白名单)

场景: XTweet JS 不再等 networkidle
  测试: x_tweet_js_omits_networkidle
  假设 调用 runcode_inline_js_x_tweet() 返回 &'static str
  当 检查字符串
  那么 不含子串 "networkidle"

场景: Default flavor JS 仍含 networkidle(回归保护)
  测试: default_js_still_contains_networkidle
  假设 调用 runcode_inline_js() 返回 &'static str
  当 检查字符串
  那么 含子串 "networkidle"

场景: build_runcode_cmd_for_url 对 x.com URL 用 XTweet JS
  测试: build_runcode_cmd_for_url_x_uses_x_tweet_js
  假设 URL = "https://x.com/yoh2_sdj/status/2055889268883796342"
  当 调用 build_runcode_cmd_for_url(url, "h", 90000, None, None)
  那么 返回的 cmd 字符串含子串 `article[data-testid=\"tweet\"]`
  并且 cmd 字符串不含子串 "networkidle"

场景: build_runcode_cmd_for_url 对非 x.com URL 用 Default JS
  测试: build_runcode_cmd_for_url_github_uses_default_js
  假设 URL = "https://github.com/oven-sh/bun/pull/30728"
  当 调用 build_runcode_cmd_for_url(url, "h", 90000, None, None)
  那么 cmd 字符串含子串 "networkidle"
  并且 cmd 字符串不含子串 `article[data-testid=\"tweet\"]`

场景: tech preset 含 x-tweet-status rule
  测试: tech_preset_has_x_tweet_status_rule
  假设 加载 bundled `presets/tech.toml`
  当 通过 route::load_preset 解析
  那么 存在 rule.kind == "x-tweet-status"
  并且 该 rule.host == "x.com"
  并且 该 rule.path_segments 等于 ["{user}", "status", "{id}"]
  并且 该 rule.executor 解析为 browser

场景: tech preset 含 x-profile rule
  测试: tech_preset_has_x_profile_rule
  假设 加载 bundled `presets/tech.toml`
  当 通过 route::load_preset 解析
  那么 存在 rule.kind == "x-profile"
  并且 该 rule.host == "x.com"
  并且 该 rule.path_segments 等于 ["{user}"]
  并且 该 rule.executor 解析为 browser

场景: tech preset 含 x-search-live rule
  测试: tech_preset_has_x_search_live_rule
  假设 加载 bundled `presets/tech.toml`
  当 通过 route::load_preset 解析
  那么 存在 rule.kind == "x-search-live"
  并且 该 rule.host == "x.com"
  并且 该 rule.executor 解析为 browser

场景: 新 rule 不破坏现有 github-issue 路由
  测试: github_issue_route_still_resolves_after_x_rules_added
  假设 加载 bundled `presets/tech.toml`
  并且 URL = "https://github.com/oven-sh/bun/issues/30719"
  当 通过 route::resolve 解析路由
  那么 命中的 rule.kind == "github-issue"
  并且 executor 解析为 postagent

## 排除范围

- 不实现 V2 server `--wait-selector` flag(actionbook-cloud 事)
- 不为 tweet / profile / search 拆 sub-flavor
- 不实现 oembed 自动 fallback(走 composite-source-fetch 后续 spec)
- 不实现 per-host runcode TOML schema 字段
- 不增加 LinkedIn / Reddit / Facebook flavor(future)
- 不修改 V1 CLI 路径
- 不修改 Default flavor JS
- 不在 V2 抓取层加任何 host-specific smell relax
- 不实现 X login state 检测(smell test 已能区分 chrome-only 与有 article
  的返回)
- 不实现 `--reseed` / `--frame-id` / `--run-code-args` 的交互(已在
  v0.4.0 spec 内,本 spec 不再覆盖)
