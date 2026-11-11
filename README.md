# Piya

A continuous AI agent with **vector memory** and an explicit **OPPAR loop** — built in Rust for a single native binary, low overhead, and predictable daemon behavior.

## How it works

```mermaid
flowchart LR
    A[Observe] --> B[Plan]
    B --> C[Pursue / Research]
    C --> D[Act]
    D --> E[Review]
    E --> F[Remember]
    F -->|retry if accuracy low| B
    F -->|done| G[Next task]
```

| Phase | What happens |
|-------|--------------|
| **Observe** | Pull next task; recall similar past work from vector memory |
| **Plan** | LLM creates steps + success criteria + research queries |
| **Pursue** | DuckDuckGo search + page fetch when external info is needed |
| **Act** | Write files and run safe commands in `workspace/` |
| **Review** | Self-critique against success criteria; score 0–1 accuracy |
| **Remember** | Store learnings, corrections, and outcomes with embeddings |

Memories are retrieved with **accuracy-weighted cosine similarity** — high-quality past work surfaces first.

## Quick start

```bash
git clone https://github.com/HeroicCommit/piya.git
cd piya

cp .env.example .env
# Set OPENAI_API_KEY in .env

cargo build --release
# binary: target/release/piya

# Or run directly
cargo run -- status
```

### Add a task

```bash
cargo run -- add "Research competitor pricing" \
  "Find pricing tiers for 3 SaaS tools in the live-chat space. Write a summary markdown file." \
  --priority 1 --tags "research,saas"
```

### Run the agent

```bash
cargo run -- run              # process all pending tasks, then exit
cargo run -- run --once       # one task
cargo run -- run --continuous # daemon (polls every 30s)
```

### SwiftUI app (Menu bar + Hummingbird)

Native menu bar + **Hummingbird** floating panel (SwiftUI + Rust via UniFFI):

```bash
./scripts/build-swift-app.sh           # stages dist/build.noindex/PiyaSwift.app (leaves /Applications untouched)
./target/release/piya app --install    # first-time install to /Applications/Piya.app + login item
```

#### Updating & Releases (no reinstall needed)

- **In-app self-update:** **Menu bar → Check for Updates…** (or **Settings → General → Check for Updates**) downloads the latest GitHub Release, verifies its SHA-256 checksum & code signature, waits for active tasks to finish, atomically swaps `/Applications/Piya.app` in place, and relaunches — keeping `.env`, `data/`, `workspace/`, and Login Item untouched.
- **CLI self-update:** `piya update` (or `piya update --check`, or `piya update --local` to promote a local staged build in place).
- **Side-by-side dev preview:** `./scripts/build-swift-app.sh --dev` builds `dist/build.noindex/PiyaDev.app` (`com.piya.swiftui.dev`) so you can test changes without stopping `/Applications/Piya.app`.
- **Publish a new release:** `./scripts/release.sh 0.2.1` (or push tag `v0.2.1` to trigger `.github/workflows/release.yml`) packages `Piya-macos-arm64.zip` + `SHA256SUMS` and creates a GitHub Release without modifying your installed app.

See [macos/README.md](macos/README.md). The legacy `tao/wry` app (`build-macos-app.sh`) still works during migration.

### macOS app (legacy wry)

Install Piya as a **native menu bar app** — no terminal for daily use.

```bash
./scripts/build-macos-app.sh           # builds dist/Piya.app
./target/release/piya app --install    # /Applications + login item
```

Open **Piya** from Applications or Spotlight. Use the menu bar icon:

| Menu | Action |
|------|--------|
| Assign Task… | Native macOS dialog |
| Run Next / Run All | Background worker (`data/piya-worker.log`) |
| View Queue… | Task summary dialog |
| Open Workspace / Project | Finder |
| About / Quit | Standard app menu |

CLI + MCP still work for OpenClaw and scripts.

**Notification Center:** When Piya.app is running, tasks trigger macOS banner popups (enable in **System Settings → Notifications → Piya**). Default `PIYA_NOTIFY=all` — use `badge` for menu bar only, `0` to disable.

### Terminal mode (optional)

```bash
./target/release/piya              # arrow-key menu
./target/release/piya watch        # TUI dashboard
./target/release/piya run --once   # colored OPPAR output
```

### Inspect state

```bash
cargo run -- status
cargo run -- list
cargo run -- list --status pending
cargo run -- recall "live chat pricing research"
```

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENAI_API_KEY` | — | Required. Any OpenAI-compatible API works |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | Base URL for compatible providers |
| `PIYA_MODEL` | `gpt-4o-mini` | Chat model |
| `PIYA_EMBEDDING_MODEL` | `text-embedding-3-small` | Embedding model for vector memory |
| `PIYA_WORKSPACE` | `./workspace` | Where artifacts are written |
| `PIYA_DATA_DIR` | `./data` | Task SQLite + LanceDB vector memory |
| `PIYA_POLL_INTERVAL_SEC` | `30` | Continuous mode poll interval |
| `PIYA_MIN_ACCURACY` | `0.7` | Pass threshold for review |
| `PIYA_MAX_RETRIES` | `3` | Re-plan attempts per task |
| `PIYA_NOTIFY` | `all` | `all` (badge + Notification Center), `badge` (menu bar only), `popup`, or `0` |

## Architecture (Rust)

```
src/
  main.rs           CLI (clap)
  config.rs         env + settings
  llm.rs            OpenAI-compatible chat + embeddings
  tasks.rs          SQLite task queue + cycle logs
  memory.rs         LanceDB vector store (cosine similarity)
  agent/
    loop_.rs        OPPAR orchestration
    planner.rs      plan / replan
    researcher.rs   web search + fetch
    executor.rs     workspace artifacts + safe commands
    reviewer.rs     self-critique + learnings
  ui/               interactive menu + TUI watch dashboard
  notify.rs         macOS notifications
```

## Data layout

```
data/
  piya.db           # tasks + cycle logs
  memory.lance/     # LanceDB vector store (auto-migrates from legacy memory.db)
workspace/          # agent output files
```

## Expertise domains

Every task is analyzed for relevant domains (auto-tagged in memory):

| Domain | Focus |
|--------|--------|
| **SEO** | Technical/on-page SEO, schema, CWV, sitemaps, WordPress/Yoast fleet |
| **Development** | Production code, APIs, refactors, Rust/Python/TS |
| **Security** | OWASP, hardening, debugging, authorized pentest-style reports |
| **Project management** | Breakdowns, timelines, risks, status docs |

Domain standards are injected into **Plan → Act → Review → Remember**. If no domain keywords match, all four apply.

## OpenClaw integration

Piya connects to OpenClaw via **MCP tools** + a **workspace skill** (same pattern as site-tracker).

```bash
./scripts/install-openclaw.sh
```

This will:
1. Build `target/release/piya`
2. Register MCP server `piya` in `~/.cursor/mcp.json`
3. Install skill to `~/.openclaw/workspace/skills/piya`
4. Enable `piya` in `~/.openclaw/openclaw.json`

**Then restart Cursor + OpenClaw gateway.**

### OpenClaw chat → Piya

In OpenClaw, delegate background work:

> Research Bunny CDN pricing and write `workspace/bunny-pricing.md`

OpenClaw should call **`piya_assign`** (MCP). Poll with **`piya_get_task`**.

| MCP tool | Purpose |
|----------|---------|
| `piya_assign` | Natural-language brief → queue + run |
| `piya_add_task` | Structured title + description |
| `piya_status` / `piya_list_tasks` | Queue visibility |
| `piya_get_task` | Poll until done/failed |
| `piya_recall` | Search vector memory |

See `openclaw/skills/piya/SKILL.md` for agent instructions.

## vs OpenClaw

| | OpenClaw | Piya |
|---|----------|------|
| Runtime | Node gateway + plugins | Single Rust binary |
| Loop | Implicit heartbeat + dreaming | Explicit OPPAR phases logged per cycle |
| Memory | Markdown promotion | Vector DB with accuracy-weighted recall |
| Tasks | Cron / chat-driven | Persistent SQLite task queue |

Point `OPENAI_BASE_URL` at your OpenClaw cursor-brain proxy if you want the same LLM backend.

## License

MIT

## Security notes

Piya runs shell commands on your machine (and can SSH to remote hosts).

- Commands outside a small read-only allow-list prompt for approval; chained commands, redirects and `$(...)` always prompt.
- `PIYA_SHELL_POLICY=allow` disables approval entirely. Only use it in a sandbox.
- Never commit `.env`, `data/` or `workspace/` (they are gitignored): they contain your API key, memories and chat history.

## License

MIT, see [LICENSE](LICENSE).
