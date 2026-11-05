---
name: piya
description: Delegate long-running work to Piya — expert in SEO, development, security/debugging, and project management. Use for research, audits, code deliverables, and structured reports in workspace/.
metadata: {"clawdbot":{"emoji":"🌸","requires":{"bins":["piya"]}}}
---

# Piya Task Worker

Piya is a **queued background agent** (Rust) with specialist expertise in:

| Domain | Examples |
|--------|----------|
| **SEO** | Technical audits, sitemaps, schema, Core Web Vitals, Yoast/WordPress fleet |
| **Development** | Code, APIs, refactors, bugs, Rust/Python/TS |
| **Security & Debugging** | OWASP, hardening, root-cause analysis — **authorized systems only** |
| **Project Management** | Breakdowns, timelines, risk registers, status reports |

Runs **OPPAR loop**: Observe → Plan → Pursue → Act → Review → Remember.

**Use Piya when:**
- SEO audit or fix list for a site (technical + on-page)
- Code, scripts, or refactors with deliverables in `workspace/`
- Security review or debugging report (systems the user owns)
- Project breakdown, roadmap, or status doc
- Work should be reviewed until quality threshold is met

**Do NOT use Piya for:**
- Quick answers in chat (answer directly)
- Real-time conversation
- Infrastructure commands (use SSH/skills directly)

## MCP tools (preferred)

When `piya` MCP server is available, use these tools:

| Tool | Purpose |
|------|---------|
| `piya_assign` | **Primary.** One natural-language brief → queued + background run |
| `piya_add_task` | Structured title + description |
| `piya_status` | Queue counts + next tasks |
| `piya_get_task` | Poll task by id until `done` / `failed` |
| `piya_list_tasks` | List queue |
| `piya_recall` | Search past learnings |
| `piya_run_once` | Process next pending task in background |

### Typical OpenClaw → Piya flow

1. **`piya_assign`** with a clear brief including deliverable path, e.g.  
   *"Research Bunny Stream pricing. Write summary to workspace/bunny-pricing.md"*
2. Tell the user the task id returned.
3. **`piya_get_task`** after ~30–120s to check status/result.
4. Read output from `workspace/` or share the result field.

### Example briefs

**SEO:**
```
Audit post-sitemap freshness for feminizationsurgeries.com after Cloudflare cache issue.
Write workspace/seo-sitemap-audit.md with P0/P1 fixes and Yoast paths to check.
```

**Development:**
```
Add a Rust function to parse WP inventory JSON and output CSV.
Write workspace/wp-inventory-export.rs and a short usage note.
```

**Security:**
```
Review auth flow in rds-backend for OWASP broken-auth patterns (read-only analysis).
Write workspace/rds-auth-review.md with severity-rated findings.
```

**Project management:**
```
Break down Open edX Tutor migration to dedicated EC2 into phases with blockers.
Write workspace/openedx-migration-plan.md — include Atlas URI blocker.
```

## CLI fallback

From the Piya project root:

```bash
./target/release/piya              # interactive menu (Mac terminal)
./target/release/piya watch        # live queue dashboard
./target/release/piya assign "..." --json
./target/release/piya run --once
./target/release/piya get <id> --json
./target/release/piya status --json
```

The user can also run tasks directly from the terminal menu without OpenClaw — use MCP for delegation from chat.

## Paths

- **Project:** your Piya checkout (`<project>`)
- **Workspace (outputs):** `<project>/workspace`
- **Data:** `<project>/data`

## vs OpenClaw

| OpenClaw | Piya |
|----------|------|
| Chat + skills | Task queue |
| Immediate replies | Background OPPAR cycles |
| Markdown memory | Vector memory + accuracy weighting |

OpenClaw **delegates**; Piya **executes** and **remembers**.
