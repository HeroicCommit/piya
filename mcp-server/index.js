#!/usr/bin/env node
/**
 * Piya MCP Server — OpenClaw / Cursor integration
 *
 * Env:
 *   PIYA_BIN      — path to piya binary (default: ../target/release/piya)
 *   PIYA_PROJECT  — project root with .env (default: parent of mcp-server)
 */

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = process.env.PIYA_PROJECT || resolve(__dirname, '..');

function resolvePiyaBin() {
  if (process.env.PIYA_BIN) return process.env.PIYA_BIN;
  for (const candidate of [
    join(PROJECT_ROOT, 'target/release/piya'),
    join(PROJECT_ROOT, 'target/debug/piya'),
  ]) {
    if (existsSync(candidate)) return candidate;
  }
  return 'piya';
}

const PIYA_BIN = resolvePiyaBin();

function log(msg) {
  process.stderr.write(`[piya-mcp] ${msg}\n`);
}

function runPiya(args, { background = false } = {}) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(PIYA_BIN, args, {
      cwd: PROJECT_ROOT,
      env: process.env,
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: background,
    });

    if (background) {
      child.unref();
      resolvePromise({ started: true, pid: child.pid, command: [PIYA_BIN, ...args].join(' ') });
      return;
    }

    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });

    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) {
        reject(new Error(stderr.trim() || `piya exited ${code}: ${stdout.trim()}`));
        return;
      }
      try {
        resolvePromise(JSON.parse(stdout));
      } catch {
        resolvePromise({ raw: stdout.trim(), stderr: stderr.trim() || undefined });
      }
    });
  });
}

function textResult(data) {
  return {
    content: [{ type: 'text', text: typeof data === 'string' ? data : JSON.stringify(data, null, 2) }],
  };
}

const server = new McpServer({ name: 'piya', version: '0.1.0' });

server.tool(
  'piya_status',
  'Queue summary: pending/in-progress/done/failed counts and next tasks',
  async () => textResult(await runPiya(['status', '--json'])),
);

server.tool(
  'piya_list_tasks',
  'List tasks in the Piya queue',
  {
    status: z.enum(['pending', 'in_progress', 'done', 'failed', 'blocked']).optional(),
    limit: z.number().int().min(1).max(100).optional(),
  },
  async ({ status, limit }) => {
    const args = ['list', '--json'];
    if (status) args.push('--status', status);
    if (limit) args.push('--limit', String(limit));
    return textResult(await runPiya(args));
  },
);

server.tool(
  'piya_get_task',
  'Get one task by full UUID or short id prefix',
  { id: z.string().describe('Task id or prefix from piya_list_tasks') },
  async ({ id }) => textResult(await runPiya(['get', id, '--json'])),
);

server.tool(
  'piya_add_task',
  'Queue a structured task for Piya OPPAR loop (research → act → review → remember)',
  {
    title: z.string(),
    description: z.string().describe('Detailed success criteria and deliverables'),
    priority: z.number().int().optional().describe('Higher runs first (default 1)'),
    tags: z.string().optional(),
    run_now: z.boolean().optional().describe('Start processing in background immediately'),
  },
  async ({ title, description, priority, tags, run_now }) => {
    const args = ['add', title, description, '--json', '--priority', String(priority ?? 1)];
    if (tags) args.push('--tags', tags);
    const task = await runPiya(args);
    if (run_now) {
      const started = await runPiya(['run', '--once'], { background: true });
      return textResult({ task, run: started });
    }
    return textResult(task);
  },
);

server.tool(
  'piya_assign',
  'Queue a task from one natural-language brief (OpenClaw chat → Piya worker)',
  {
    brief: z.string().describe('What to research/build; include output path in workspace/ if needed'),
    priority: z.number().int().optional(),
    tags: z.string().optional(),
    run_now: z.boolean().optional().describe('Start processing in background (default true)'),
  },
  async ({ brief, priority, tags, run_now }) => {
    const args = ['assign', brief, '--json', '--priority', String(priority ?? 1)];
    if (tags) args.push('--tags', tags);
    const task = await runPiya(args);
    if (run_now !== false) {
      const started = await runPiya(['run', '--once'], { background: true });
      return textResult({ task, run: started, note: 'Poll piya_get_task until status is done or failed' });
    }
    return textResult(task);
  },
);

server.tool(
  'piya_run_once',
  'Process the next pending task in the background (OPPAR loop)',
  {},
  async () => textResult(await runPiya(['run', '--once'], { background: true })),
);

server.tool(
  'piya_recall',
  'Semantic search over Piya vector memory (past learnings and outcomes)',
  {
    query: z.string(),
    limit: z.number().int().min(1).max(20).optional(),
  },
  async ({ query, limit }) => {
    const args = ['recall', query, '--json'];
    if (limit) args.push('--limit', String(limit));
    return textResult(await runPiya(args));
  },
);

log(`Using piya binary: ${PIYA_BIN}`);
log(`Project root: ${PROJECT_ROOT}`);

const transport = new StdioServerTransport();
await server.connect(transport);
