/// Domain expertise injected into OPPAR phases that actually match the task.
/// "Hacking" = security engineering, debugging, root-cause analysis — authorized systems only.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Domain {
    Seo,
    Development,
    Security,
    ProjectManagement,
}

impl Domain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Seo => "seo",
            Self::Development => "development",
            Self::Security => "security",
            Self::ProjectManagement => "project_management",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Seo => "SEO",
            Self::Development => "Development",
            Self::Security => "Security & Debugging",
            Self::ProjectManagement => "Project Management",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Self::Seo => SEO,
            Self::Development => DEVELOPMENT,
            Self::Security => SECURITY,
            Self::ProjectManagement => PROJECT_MANAGEMENT,
        }
    }
}

/// Infer relevant domains from task title + description.
/// Unmatched tasks get no specialist dump — only cross-cutting standards.
pub fn detect_domains(title: &str, description: &str) -> Vec<Domain> {
    let text = format!("{title} {description}").to_lowercase();
    let mut domains = Vec::new();

    let seo_kw = [
        "seo", "sitemap", "meta tag", "schema.org", "structured data", "canonical", "robots.txt",
        "crawl", "indexing", "ranking", "keyword", "backlink", "yoast", "core web vitals", "lcp",
        "cls", "inp", "og:", "twitter card", "hreflang", "snippet", "serp", "search console",
        "googlebot", "noindex", "wordpress seo",
    ];
    let dev_kw = [
        "code", "implement", "refactor", "bug", "fix", "api", "endpoint", "database", "rust",
        "python", "typescript", "javascript", "react", "next.js", "nestjs", "docker", "test",
        "unit test", "ci", "deploy", "git", "function", "class", "module", "compile", "build",
        "frontend", "backend", "script", "migration",
    ];
    let sec_kw = [
        "security", "vuln", "xss", "csrf", "sqli", "injection", "auth", "oauth", "jwt", "ssl",
        "tls", "pentest", "harden", "exploit", "cve", "owasp", "debug", "root cause", "trace",
        "log analysis", "recon", "footprint", "privilege", "sanitize", "encrypt", "secret",
        "hack", "hacking", "infosec",
    ];
    let pm_kw = [
        "project", "timeline", "milestone", "sprint", "backlog", "roadmap", "stakeholder",
        "priority", "scope", "deliverable", "estimate", "deadline", "kanban", "risk register",
        "requirements", "spec", "breakdown", "workstream", "gantt", "status report",
    ];

    if seo_kw.iter().any(|k| text.contains(k)) {
        domains.push(Domain::Seo);
    }
    if dev_kw.iter().any(|k| text.contains(k)) {
        domains.push(Domain::Development);
    }
    if sec_kw.iter().any(|k| text.contains(k)) {
        domains.push(Domain::Security);
    }
    if pm_kw.iter().any(|k| text.contains(k)) {
        domains.push(Domain::ProjectManagement);
    }

    domains.sort_by_key(|d| d.as_str());
    domains.dedup();
    domains
}

pub fn domains_to_tags(domains: &[Domain]) -> String {
    domains
        .iter()
        .map(|d| d.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

pub fn format_domain_context(domains: &[Domain]) -> String {
    if domains.is_empty() {
        return format!("No specialist domain matched. {CROSS_CUTTING}");
    }
    let labels: Vec<_> = domains.iter().map(|d| d.label()).collect();
    let mut out = format!(
        "Active expertise for this task: {}.\nApply only these standards.\n\n",
        labels.join(", ")
    );
    for domain in domains {
        out.push_str(domain.section());
        out.push('\n');
    }
    out.push_str(CROSS_CUTTING);
    out
}

const SEO: &str = r#"### SEO
- Technical: crawlability, indexation, canonicals, hreflang, robots.txt, XML sitemaps.
- On-page: title/meta length, H1-H6, internal links, alt, schema.org JSON-LD.
- Performance: LCP, INP, CLS, mobile-first.
- WordPress/Yoast: sitemap freshness, Cloudflare cache on sitemaps.
- Deliverables: P0/P1/P2 checklists with impact × effort.
"#;

const DEVELOPMENT: &str = r#"### Development
- Production code: clear names, error handling, tests where valuable, minimal scope.
- Match the stack in context (Rust, Python, TypeScript/Node, PHP/WordPress).
- Root-cause over patches; document assumptions.
- Safe refactors: read before write, preserve behavior, cite files.
"#;

const SECURITY: &str = r#"### Security & debugging
- ONLY systems the user owns or has authorized. No attack steps against third parties.
- Threat model, OWASP Top 10, least privilege, secrets out of git.
- Debug: reproduce → isolate → verify → fix.
- Reports: finding, severity, evidence, remediation.
"#;

const PROJECT_MANAGEMENT: &str = r#"### Project management
- Measurable deliverables and done criteria.
- Impact × effort; name blockers, dependencies, risks.
- Status: what changed, what's next, what's blocked.
- Flag scope creep; propose phases when large.
"#;

const CROSS_CUTTING: &str = r#"Cite sources for facts. Prefer markdown with headings and checklists. Be specific: filenames, commands, metrics."#;

pub fn planner_system() -> String {
    r#"You are Piya, a task planner. Produce a concrete plan as JSON:
{
  "goal": "one sentence",
  "domains": ["seo|development|security|project_management"],
  "success_criteria": ["measurable criterion"],
  "research_queries": ["web search query if external info is needed, else empty"],
  "steps": [
    {"step": 1, "action": "what to do", "output": "expected artifact"}
  ],
  "risks": ["potential failure mode"],
  "learnings_to_apply": ["how past memory should guide this work"]
}
Keep research_queries empty when the work is local, recurring, or already has a script."#.into()
}

pub fn executor_system() -> String {
    r#"You are Piya executing a task in the workspace.

Tools:
- write_file — create/update a workspace file
- read_file — inspect an existing workspace file
- run_command — run a safe shell command and read stdout

Rules:
- Use tools. Do not claim success unless a tool result shows it.
- If a script or report already exists for this job, run or update it — do not recreate the project.
- Paths are relative to the workspace.
- No rm -rf, curl|bash, or sudo.
- When done, stop calling tools and reply with JSON only:
{"summary": "what was accomplished", "notes": "implementation details"}"#
        .into()
}

pub fn reviewer_system() -> String {
    r#"You are Piya's quality reviewer. Score the work against the plan's success criteria.
Output JSON:
{
  "accuracy": 0.0-1.0,
  "passed": true/false,
  "criteria_results": [
    {"criterion": "...", "met": true/false, "evidence": "..."}
  ],
  "strengths": ["..."],
  "gaps": ["..."],
  "corrections": ["specific fix for next attempt"],
  "learnings": [
    {"text": "durable insight", "kind": "learning|correction|pattern"}
  ],
  "should_retry": true/false
}
Be strict but fair. accuracy >= 0.85 excellent; 0.7 acceptable; below 0.7 should_retry.
Penalize invented results, unused command stdout, and rewriting a job that already had a working script."#.into()
}

pub fn researcher_system() -> String {
    r#"You are a research analyst. Extract only facts relevant to the task from raw search results. Output JSON:
{
  "summary": "concise synthesis",
  "key_facts": ["fact1", "fact2"],
  "gaps": ["what is still unknown"],
  "confidence": 0.0-1.0,
  "sources": ["url or source name"]
}"#.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_task_gets_no_domains() {
        let domains = detect_domains(
            "Every day at 6:30 am, send the daily stats report.",
            "Every day at 6:30 am, send the daily stats report.",
        );
        assert!(domains.is_empty(), "{domains:?}");
        let ctx = format_domain_context(&domains);
        assert!(!ctx.contains("SEO Expert") && !ctx.contains("### SEO"));
        assert!(ctx.contains("No specialist domain matched"));
    }

    #[test]
    fn seo_task_only_gets_seo() {
        let domains = detect_domains("Audit sitemap", "Yoast XML sitemap cache on WordPress");
        assert_eq!(domains, vec![Domain::Seo]);
        let ctx = format_domain_context(&domains);
        assert!(ctx.contains("### SEO"));
        assert!(!ctx.contains("### Development"));
    }
}
