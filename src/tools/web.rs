//! Web tools: web_fetch (URL to readable text) and web_search backends.

use crate::api::{truncate, MAX_TOOL_OUTPUT};
use std::sync::OnceLock;
use std::time::Duration;

// ----------------------------------------------------------- web_fetch ------

/// Truncate `s` to at most `max_bytes`, snapping down to the nearest char
/// boundary. `String::truncate` panics when the byte index splits a multibyte
/// codepoint — common on real >2MB web pages — so callers go through here.
fn truncate_to_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Fetch a URL and return readable text. HTML is stripped to text; anything
/// else (JSON, plain text, …) comes back as-is. Body capped at 2MB.
pub fn web_fetch(http: &ureq::Agent, url: &str) -> String {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return "ERROR: url must start with http:// or https://".into();
    }
    let resp = http
        .get(url)
        .timeout(Duration::from_secs(10))
        .set("Accept", "text/html, application/json, text/plain, */*")
        .call();
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            return format!("ERROR: HTTP {code}: {}", truncate(&body, 500));
        }
        Err(e) => return format!("ERROR: {e}"),
    };
    let html = resp
        .content_type()
        .to_ascii_lowercase()
        .contains("text/html");
    // Use into_string() (not into_reader()) so ureq's timeout covers the body
    // read — prevents slow-loris attacks where a server dribbles bytes.
    let mut body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => return format!("ERROR: read failed (binary?): {e}"),
    };
    truncate_to_char_boundary(&mut body, 2_000_000);
    let text = if html { html_to_text(&body) } else { body };
    let text = text.trim();
    if text.is_empty() {
        "(empty response)".into()
    } else {
        truncate(text, MAX_TOOL_OUTPUT)
    }
}

/// Best-effort HTML → text: drop script/style/head, turn block-level closes
/// into newlines, strip the remaining tags, decode common entities.
fn html_to_text(html: &str) -> String {
    // Lift regex construction into OnceLock statics so they compile once.
    static BLOCKS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    static BREAKS: OnceLock<regex::Regex> = OnceLock::new();
    static ANY_TAG: OnceLock<regex::Regex> = OnceLock::new();
    let blocks = BLOCKS.get_or_init(|| {
        ["script", "style", "head", "noscript", "svg"]
            .iter()
            .map(|tag| {
                regex::RegexBuilder::new(&format!(r"<{tag}\b.*?</{tag}>"))
                    .case_insensitive(true)
                    .dot_matches_new_line(true)
                    .build()
                    .unwrap()
            })
            .collect()
    });
    let mut s = html.to_string();
    for re in blocks {
        s = re.replace_all(&s, " ").into_owned();
    }
    let breaks = BREAKS.get_or_init(|| {
        regex::RegexBuilder::new(r"</(p|div|li|tr|h[1-6]|blockquote|pre)>|<br\s*/?>")
            .case_insensitive(true)
            .build()
            .unwrap()
    });
    let s = breaks.replace_all(&s, "\n");
    let tags = ANY_TAG.get_or_init(|| regex::Regex::new(r"<[^>]*>").unwrap());
    let s = decode_entities(&tags.replace_all(&s, " "));
    // Collapse runs of spaces and blank lines left behind by stripped markup.
    let mut out = String::new();
    let mut blank_run = 0;
    for line in s.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}

// ----------------------------------------------------------- web_search -----

/// Where web_search goes. `"web_search"` in config/settings picks one
/// (`searxng` / `brave` / `tavily` / `deepseek` / `duckduckgo`); unset or
/// `"auto"` takes the first that's configured — a `web_search_url` (SearXNG),
/// TAVILY_API_KEY, BRAVE_API_KEY — and otherwise keyless DuckDuckGo.
/// `web_search_key` stands in for the env key. DeepSeek's Search API (billed
/// per request) is opt-in only; its key falls back to the DeepSeek chat key.
#[derive(Debug, PartialEq)]
enum SearchBackend {
    Searxng(String),
    Brave(String),
    Tavily(String),
    DeepSeek(String),
    DuckDuckGo,
}

fn search_backend(
    choice: Option<&str>,
    url: Option<String>,
    key: Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> std::result::Result<SearchBackend, String> {
    let need = |what: &str, v: Option<String>| v.ok_or(format!("ERROR: web_search is set to {what}"));
    match choice.unwrap_or("auto").to_ascii_lowercase().as_str() {
        "searxng" => Ok(SearchBackend::Searxng(need("searxng but web_search_url isn't set", url)?)),
        "brave" => Ok(SearchBackend::Brave(need("brave but BRAVE_API_KEY isn't set", key.or_else(|| env("BRAVE_API_KEY")))?)),
        "tavily" => {
            Ok(SearchBackend::Tavily(need("tavily but TAVILY_API_KEY isn't set", key.or_else(|| env("TAVILY_API_KEY")))?))
        }
        "deepseek" => Ok(SearchBackend::DeepSeek(need(
            "deepseek but there's no DeepSeek API key",
            key.or_else(|| env("DEEPSEEK_API_KEY")),
        )?)),
        "duckduckgo" | "ddg" => Ok(SearchBackend::DuckDuckGo),
        _ => Ok(if let Some(u) = url {
            SearchBackend::Searxng(u)
        } else if let Some(k) = env("TAVILY_API_KEY") {
            SearchBackend::Tavily(k)
        } else if let Some(k) = env("BRAVE_API_KEY") {
            SearchBackend::Brave(k)
        } else {
            SearchBackend::DuckDuckGo
        }),
    }
}

/// Search the web and return numbered "title / url / snippet" rows.
pub fn web_search(http: &ureq::Agent, query: &str) -> String {
    if query.trim().is_empty() {
        return "ERROR: empty query".into();
    }
    let s = |k: &str| crate::config::setting(k).and_then(|v| v.as_str().map(str::to_string)).filter(|v| !v.trim().is_empty());
    let env = |k: &str| {
        std::env::var(k).ok().filter(|v| !v.trim().is_empty()).or_else(|| {
            // DeepSeek search reuses the chat key when DeepSeek is the provider.
            (k == "DEEPSEEK_API_KEY")
                .then(crate::config::Config::load)
                .filter(|c| c.provider == "deepseek" && !c.api_key.trim().is_empty())
                .map(|c| c.api_key)
        })
    };
    let backend = match search_backend(s("web_search").as_deref(), s("web_search_url"), s("web_search_key"), env) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let q = urlencode(query);
    let get = |url: &str| http.get(url).timeout(Duration::from_secs(30)).set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) haikucode/0.3");
    let (name, resp) = match &backend {
        SearchBackend::Searxng(base) => {
            ("SearXNG", get(&format!("{}/search?q={q}&format=json", base.trim_end_matches('/'))).call())
        }
        SearchBackend::Brave(key) => (
            "Brave",
            get(&format!("https://api.search.brave.com/res/v1/web/search?q={q}&count=8"))
                .set("Accept", "application/json")
                .set("X-Subscription-Token", key)
                .call(),
        ),
        SearchBackend::Tavily(key) => (
            "Tavily",
            http.post("https://api.tavily.com/search")
                .timeout(Duration::from_secs(30))
                .set("Authorization", &format!("Bearer {key}"))
                .set("Content-Type", "application/json")
                .send_string(&serde_json::json!({ "query": query, "max_results": 8 }).to_string()),
        ),
        SearchBackend::DeepSeek(key) => (
            "DeepSeek",
            http.post("https://api.deepseek.com/v1/tools/search")
                .timeout(Duration::from_secs(30))
                .set("Authorization", &format!("Bearer {key}"))
                .set("Content-Type", "application/json")
                .send_string(&serde_json::json!({ "query": deepseek_query(query), "count": 8 }).to_string()),
        ),
        SearchBackend::DuckDuckGo => ("DuckDuckGo", get(&format!("https://html.duckduckgo.com/html/?q={q}")).call()),
    };
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let hint = match (code, &backend) {
                (403, SearchBackend::Searxng(_)) => " (enable the json format in the instance's settings.yml)",
                (402, SearchBackend::DeepSeek(_)) => " (insufficient DeepSeek balance)",
                (401 | 403, _) => " (check the API key)",
                (429, _) => " (rate limited; try again shortly)",
                _ => "",
            };
            let _ = r.into_string();
            return format!("ERROR: {name} search returned HTTP {code}{hint}");
        }
        Err(e) => return format!("ERROR: {name} search failed: {e}"),
    };
    // DuckDuckGo answers a rate-limited client with 202 and a bot check.
    let status = resp.status();
    // into_string() keeps ureq's deadline so a slow server can't hang us.
    let mut body = resp.into_string().unwrap_or_default();
    truncate_to_char_boundary(&mut body, 2_000_000);
    match backend {
        SearchBackend::DuckDuckGo if status == 202 || ddg_blocked(&body) => DDG_BLOCKED.to_string(),
        SearchBackend::DuckDuckGo => parse_ddg(&body),
        SearchBackend::Searxng(_) => format_hits(name, parse_json_hits(&body, "/results", "content")),
        SearchBackend::Tavily(_) => format_hits(name, parse_json_hits(&body, "/results", "content")),
        SearchBackend::Brave(_) => format_hits(name, parse_json_hits(&body, "/web/results", "description")),
        SearchBackend::DeepSeek(_) => format_hits(name, parse_json_hits(&body, "/results", "summary")),
    }
}

/// DeepSeek's Search API takes 1-128 characters; trim longer queries on a
/// char boundary rather than have the request rejected.
fn deepseek_query(query: &str) -> String {
    query.trim().chars().take(128).collect()
}

const DDG_BLOCKED: &str = "ERROR: DuckDuckGo is rate-limiting this connection with a bot check, so there are no \
results. Wait a few minutes, or configure another search backend: \"web_search_url\" (a SearXNG \
instance), BRAVE_API_KEY or TAVILY_API_KEY.";

/// DuckDuckGo's anomaly / bot-check page instead of results.
fn ddg_blocked(html: &str) -> bool {
    ["anomaly-modal", "anomaly.js", "bots use DuckDuckGo too", "challenge-form"].iter().any(|m| html.contains(m))
}

/// (title, url, snippet) rows from a JSON search response: the array at
/// `pointer`, each item's `title` / `url` and `snippet_key` text.
fn parse_json_hits(body: &str, pointer: &str, snippet_key: &str) -> std::result::Result<Vec<(String, String, String)>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "the response wasn't JSON (for SearXNG, enable the json format)".to_string())?;
    let items = v.pointer(pointer).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|it| {
            let s = |k: &str| it.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let url = s("url");
            (!url.is_empty()).then(|| (clean_inline(&s("title")), url, clean_inline(&s(snippet_key))))
        })
        .take(8)
        .collect())
}

fn format_hits(name: &str, hits: std::result::Result<Vec<(String, String, String)>, String>) -> String {
    let hits = match hits {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {name} search: {e}"),
    };
    if hits.is_empty() {
        return format!("(no results from {name})");
    }
    let mut out = String::new();
    for (i, (title, url, snippet)) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {title}\n   {url}\n", i + 1));
        if !snippet.is_empty() {
            out.push_str(&format!("   {snippet}\n"));
        }
    }
    truncate(&out, MAX_TOOL_OUTPUT)
}

fn parse_ddg(html: &str) -> String {
    let re = |p: &str| {
        regex::RegexBuilder::new(p)
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build()
            .unwrap()
    };
    // Match DDG result links — look for uddg= redirect URLs (the stable
    // structural element) and extract the anchor text as title.  This is
    // more robust than depending on specific class names.
    let link_re = re(r#"<a[^>]*href="[^"]*uddg=[^"]*"[^>]*>(.*?)</a>"#);
    // For snippets, try any element whose class contains "snippet".
    let snip_re = re(r#"class="[^"]*snippet[^"]*"[^>]*>(.*?)</(?:a|td|div|span)>"#);
    let snippets: Vec<String> = snip_re
        .captures_iter(html)
        .map(|c| clean_inline(&c[1]))
        .collect();
    let mut out = String::new();
    for (i, c) in link_re.captures_iter(html).take(8).enumerate() {
        let title = clean_inline(&c[1]);
        // Extract uddg= value from the full match.
        let full = &c[0];
        let href_start = full.find("uddg=").map(|p| p + 5).unwrap_or(0);
        let href_rest = &full[href_start..];
        let href_end = href_rest.find('"').unwrap_or(href_rest.len());
        let encoded = &href_rest[..href_end];
        // The uddg value ends at the next query param (`&rut=…`); keep only the
        // encoded destination so the URL doesn't carry a tracking suffix.
        let encoded = &encoded[..encoded.find('&').unwrap_or(encoded.len())];
        out.push_str(&format!("{}. {title}\n   {}\n", i + 1, urldecode(encoded)));
        if let Some(s) = snippets.get(i) {
            if !s.is_empty() {
                out.push_str(&format!("   {s}\n"));
            }
        }
    }
    if out.is_empty() {
        "(no results — DuckDuckGo may have changed its output format)".into()
    } else {
        truncate(&out, MAX_TOOL_OUTPUT)
    }
}

/// DDG result hrefs are redirects like `//duckduckgo.com/l/?uddg=<encoded>&…`;
/// pull out and decode the real destination.
#[cfg(test)]
fn resolve_ddg_url(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let rest = &href[pos + 5..];
        let end = rest.find('&').unwrap_or(rest.len());
        return urldecode(&rest[..end]);
    }
    if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        href.to_string()
    }
}

/// Strip tags and entities from an inline HTML fragment, collapsing whitespace.
fn clean_inline(s: &str) -> String {
    static ANY_TAG: OnceLock<regex::Regex> = OnceLock::new();
    let tags = ANY_TAG.get_or_init(|| regex::Regex::new(r"<[^>]*>").unwrap());
    decode_entities(&tags.replace_all(s, ""))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_markup() {
        let html = "<html><head><title>T</title><style>x{color:red}</style></head>\
                    <body><script>var a=1;</script><h1>Hello</h1>\
                    <p>World &amp; friends</p><ul><li>one</li><li>two</li></ul></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World & friends"));
        assert!(text.contains("one"));
        assert!(!text.contains("var a"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn web_fetch_rejects_non_http() {
        let http = crate::api::agent_http();
        assert!(web_fetch(&http, "file:///etc/passwd").starts_with("ERROR"));
        assert!(web_fetch(&http, "ftp://x").starts_with("ERROR"));
    }

    #[test]
    fn url_encode_decode_roundtrip() {
        let s = "rust ureq & \"tools\" ~/.config";
        assert_eq!(urldecode(&urlencode(s)), s);
        assert_eq!(urlencode("a b"), "a+b");
        assert_eq!(urldecode("https%3A%2F%2Fexample.com%2Fx"), "https://example.com/x");
    }

    #[test]
    fn ddg_redirect_resolution() {
        assert_eq!(
            resolve_ddg_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&rut=abc"),
            "https://example.com/docs"
        );
        assert_eq!(resolve_ddg_url("https://direct.example.com"), "https://direct.example.com");
    }

    #[test]
    fn ddg_parse_fixture() {
        let html = r#"<div class="result"><h2 class="result__title">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&rut=x">Example <b>Domain</b></a></h2>
            <a class="result__snippet" href="x">This domain is for use in &amp;c.</a></div>"#;
        let out = parse_ddg(html);
        assert!(out.contains("1. Example Domain"), "got: {out}");
        assert!(out.contains("https://example.com/"));
        // The redirect's trailing tracking param (&rut=…) must be stripped.
        assert!(!out.contains("rut"), "tracking suffix leaked into url: {out}");
        assert!(out.contains("This domain is for use in &c."));
    }

    #[test]
    fn web_search_backend_is_picked_by_setting_or_available_key() {
        let none = |_: &str| None::<String>;
        let tavily = |k: &str| (k == "TAVILY_API_KEY").then(|| "tk".to_string());
        assert_eq!(search_backend(None, None, None, none), Ok(SearchBackend::DuckDuckGo));
        assert_eq!(search_backend(Some("auto"), None, None, tavily), Ok(SearchBackend::Tavily("tk".into())));
        assert_eq!(
            search_backend(None, Some("https://sx.local".into()), None, tavily),
            Ok(SearchBackend::Searxng("https://sx.local".into())),
            "a configured SearXNG wins in auto"
        );
        assert_eq!(search_backend(Some("Brave"), None, Some("bk".into()), none), Ok(SearchBackend::Brave("bk".into())));
        assert!(search_backend(Some("brave"), None, None, none).unwrap_err().contains("BRAVE_API_KEY"));
        assert!(search_backend(Some("searxng"), None, None, none).unwrap_err().contains("web_search_url"));
        assert_eq!(search_backend(Some("ddg"), None, None, tavily), Ok(SearchBackend::DuckDuckGo));
        // DeepSeek is opt-in (billed per request), never picked by auto.
        let ds = |k: &str| (k == "DEEPSEEK_API_KEY").then(|| "dk".to_string());
        assert_eq!(search_backend(None, None, None, ds), Ok(SearchBackend::DuckDuckGo));
        assert_eq!(search_backend(Some("deepseek"), None, None, ds), Ok(SearchBackend::DeepSeek("dk".into())));
        assert!(search_backend(Some("deepseek"), None, None, none).unwrap_err().contains("DeepSeek API key"));
        assert_eq!(deepseek_query(&"é".repeat(200)).chars().count(), 128);
    }

    #[test]
    fn search_responses_parse_per_backend() {
        // The example from DeepSeek's Search API reference: `summary` is the
        // model-facing text (`snippet` is a shorter one for UIs).
        let ds = r#"{"results":[{"rerank_score":2.4,"title":"DeepSeek V4 model released","url":"https://example.com/deepseek-v4","snippet":"short","summary":"DeepSeek has released the V4 model.","content":"","site_name":"Example News","site_icon":null,"date":"2026-06-01T00:00:00+00:00","authority":3}]}"#;
        assert_eq!(
            format_hits("DeepSeek", parse_json_hits(ds, "/results", "summary")),
            "1. DeepSeek V4 model released\n   https://example.com/deepseek-v4\n   DeepSeek has released the V4 model.\n"
        );
        assert_eq!(format_hits("DeepSeek", parse_json_hits(r#"{"results":[]}"#, "/results", "summary")), "(no results from DeepSeek)");
        let searx = r#"{"query":"rust","results":[{"title":"The Rust Book","url":"https://doc.rust-lang.org/book/","content":"Learn Rust."},{"title":"no url"}]}"#;
        let out = format_hits("SearXNG", parse_json_hits(searx, "/results", "content"));
        assert_eq!(out, "1. The Rust Book\n   https://doc.rust-lang.org/book/\n   Learn Rust.\n");
        let brave = r#"{"web":{"results":[{"title":"Rust &amp; Cargo","url":"https://rust-lang.org","description":"A <strong>language</strong> empowering everyone"}]}}"#;
        let out = format_hits("Brave", parse_json_hits(brave, "/web/results", "description"));
        assert!(out.contains("1. Rust & Cargo") && out.contains("A language empowering everyone"), "{out}");
        let tavily = r#"{"results":[{"title":"T","url":"https://t.example","content":"snippet","score":0.9}]}"#;
        assert!(format_hits("Tavily", parse_json_hits(tavily, "/results", "content")).starts_with("1. T\n   https://t.example"));
        assert_eq!(format_hits("Brave", parse_json_hits(r#"{"web":{"results":[]}}"#, "/web/results", "description")), "(no results from Brave)");
        let html = format_hits("SearXNG", parse_json_hits("<html>no json</html>", "/results", "content"));
        assert!(html.starts_with("ERROR: SearXNG search:") && html.contains("json format"), "{html}");
    }

    #[test]
    fn duckduckgo_bot_check_is_reported_not_swallowed() {
        let page = r#"<html><body><div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div><form id="challenge-form"></form></body></html>"#;
        assert!(ddg_blocked(page));
        assert!(!ddg_blocked(r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com">Ex</a>"#));
        assert!(DDG_BLOCKED.contains("rate-limiting") && DDG_BLOCKED.contains("web_search_url"));
    }

    #[test]
    #[ignore] // hits the network; run with `cargo test -- --ignored`
    fn web_search_real() {
        let http = crate::api::agent_http();
        let out = web_search(&http, "rust programming language");
        assert!(out.contains("rust-lang.org"), "got: {out}");
    }

    #[test]
    #[ignore] // hits the network; run with `cargo test -- --ignored`
    fn web_fetch_real_page() {
        let http = crate::api::agent_http();
        let text = web_fetch(&http, "https://example.com");
        assert!(text.contains("Example Domain"), "got: {text}");
        assert!(!text.contains('<'));
    }
}
