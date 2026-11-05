use anyhow::Result;
use futures::future::join_all;
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::agent::expertise::{self, Domain};
use crate::llm::LlmClient;

pub struct Researcher {
    llm: LlmClient,
    http: Client,
}

#[derive(Clone)]
struct Finding {
    title: String,
    snippet: String,
    url: Option<String>,
}

impl Researcher {
    pub fn new(llm: LlmClient) -> Self {
        Self {
            llm,
            http: Client::builder()
                .user_agent("PiyaAgent/0.1")
                .redirect(reqwest::redirect::Policy::limited(5))
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("build HTTP client"),
        }
    }

    pub async fn research(&self, task: &str, queries: &[String], domains: &[Domain]) -> Result<Value> {
        let searches = join_all(queries.iter().take(4).map(|q| self.search_web(q))).await;
        let items: Vec<Finding> = searches.into_iter().flatten().collect();

        let http = self.http.clone();
        let pages = join_all(items.iter().map(|finding| {
            let http = http.clone();
            let url = finding.url.clone();
            async move {
                match url {
                    Some(u) => Some(fetch_url_with(&http, &u).await),
                    None => None,
                }
            }
        }))
        .await;

        let mut raw_blocks = Vec::new();
        for (finding, page) in items.iter().zip(pages) {
            let mut block = format!("### {}\n{}", finding.title, finding.snippet);
            if let Some(url) = &finding.url {
                block.push_str(&format!("\nURL: {url}"));
                if let Some(page) = page {
                    block.push_str(&format!(
                        "\n\nPage excerpt:\n{}",
                        &page[..page.len().min(3000)]
                    ));
                }
            }
            raw_blocks.push(block);
        }

        if raw_blocks.is_empty() {
            return Ok(json!({
                "summary": "No web results found.",
                "key_facts": [],
                "gaps": ["Unable to find external sources"],
                "confidence": 0.2
            }));
        }

        let user = format!(
            "{}\n\nTask:\n{task}\n\nRaw research:\n\n{}",
            expertise::format_domain_context(domains),
            raw_blocks.join("\n\n---\n\n")
        );
        self.llm.chat_json(&expertise::researcher_system(), &user).await
    }

    pub fn format_for_prompt(research: &Value) -> String {
        let facts = research
            .get("key_facts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let gaps = research
            .get("gaps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut lines = vec![
            format!(
                "Summary: {}",
                research.get("summary").and_then(|v| v.as_str()).unwrap_or("")
            ),
            format!(
                "Confidence: {}",
                research.get("confidence").map(|v| v.to_string()).unwrap_or_else(|| "0".into())
            ),
            "Key facts:".into(),
        ];
        for fact in facts {
            if let Some(s) = fact.as_str() {
                lines.push(format!("- {s}"));
            }
        }
        if !gaps.is_empty() {
            lines.push("Gaps:".into());
            for gap in gaps {
                if let Some(s) = gap.as_str() {
                    lines.push(format!("- {s}"));
                }
            }
        }
        lines.join("\n")
    }

    async fn search_web(&self, query: &str) -> Vec<Finding> {
        let mut findings = self.search_ddg_api(query).await;
        if findings.is_empty() {
            findings = self.search_ddg_html(query).await;
        }
        findings
    }

    async fn search_ddg_api(&self, query: &str) -> Vec<Finding> {
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_redirect=1&no_html=1",
            urlencoding(query)
        );
        let Ok(resp) = self.http.get(&url).send().await else {
            return vec![];
        };
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return vec![];
        };

        let mut findings = Vec::new();
        if let Some(text) = body.get("AbstractText").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                findings.push(Finding {
                    title: body
                        .get("Heading")
                        .and_then(|v| v.as_str())
                        .unwrap_or(query)
                        .to_string(),
                    snippet: text.to_string(),
                    url: body
                        .get("AbstractURL")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                });
            }
        }

        if let Some(topics) = body.get("RelatedTopics").and_then(|v| v.as_array()) {
            for topic in topics.iter().take(4) {
                if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                    findings.push(Finding {
                        title: topic
                            .get("FirstURL")
                            .and_then(|v| v.as_str())
                            .unwrap_or("related")
                            .to_string(),
                        snippet: text.to_string(),
                        url: topic
                            .get("FirstURL")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    });
                }
            }
        }

        findings
    }

    async fn search_ddg_html(&self, query: &str) -> Vec<Finding> {
        let response = match self
            .http
            .post("https://html.duckduckgo.com/html/")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("q={}", urlencoding(query)))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return vec![Finding {
                    title: "Search failed".into(),
                    snippet: e.to_string(),
                    url: None,
                }];
            }
        };

        let html = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return vec![Finding {
                    title: "Search failed".into(),
                    snippet: e.to_string(),
                    url: None,
                }];
            }
        };

        parse_ddg_html(&html).into_iter().take(5).collect()
    }
}

async fn fetch_url_with(http: &Client, url: &str) -> String {
    match http.get(url).send().await {
        Ok(resp) => match resp.text().await {
            Ok(text) => strip_html(&text, 8000),
            Err(e) => format!("Failed to read {url}: {e}"),
        },
        Err(e) => format!("Failed to fetch {url}: {e}"),
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

fn parse_ddg_html(html: &str) -> Vec<Finding> {
    static SELECTORS: OnceLock<Option<(Selector, Selector, Selector)>> = OnceLock::new();
    let Some((result_sel, title_sel, snippet_sel)) = SELECTORS.get_or_init(|| {
        Some((
            Selector::parse(".result").ok()?,
            Selector::parse(".result__a").ok()?,
            Selector::parse(".result__snippet").ok()?,
        ))
    }) else {
        return vec![];
    };

    let document = Html::parse_document(html);
    document
        .select(result_sel)
        .filter_map(|result| {
            let title = result
                .select(title_sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())?;
            let url = result
                .select(title_sel)
                .next()
                .and_then(|el| el.value().attr("href"))
                .map(str::to_string);
            let snippet = result
                .select(snippet_sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .unwrap_or_default();
            Some(Finding {
                title,
                snippet,
                url,
            })
        })
        .collect()
}

fn html_regexes() -> &'static (Regex, Regex, Regex, Regex) {
    static RE: OnceLock<(Regex, Regex, Regex, Regex)> = OnceLock::new();
    RE.get_or_init(|| {
        (
            Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("script regex"),
            Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("style regex"),
            Regex::new(r"<[^>]+>").expect("tag regex"),
            Regex::new(r"\s+").expect("space regex"),
        )
    })
}

fn strip_html(html: &str, max_chars: usize) -> String {
    let (script_re, style_re, tag_re, space_re) = html_regexes();
    let text = script_re.replace_all(html, " ");
    let text = style_re.replace_all(&text, " ");
    let text = tag_re.replace_all(&text, " ");
    let text = space_re.replace_all(&text, " ").trim().to_string();
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::strip_html;

    #[test]
    fn strips_tags_and_scripts() {
        let html = "<html><script>alert(1)</script><p>Hello   world</p></html>";
        let out = strip_html(html, 80);
        assert!(out.contains("Hello world"));
        assert!(!out.contains("alert"));
        assert!(!out.contains("<p>"));
    }
}
