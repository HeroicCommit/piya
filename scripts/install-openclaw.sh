#!/usr/bin/env bash
# Wire Piya into OpenClaw: MCP server + workspace skill
set -euo pipefail

PIYA_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MCP_JSON="$HOME/.cursor/mcp.json"
SKILL_SRC="$PIYA_ROOT/openclaw/skills/piya"
SKILL_DST="$HOME/.openclaw/workspace/skills/piya"
PIYA_BIN="$PIYA_ROOT/target/release/piya"

echo "==> Building Piya release binary..."
(cd "$PIYA_ROOT" && cargo build --release)

echo "==> Installing MCP server deps..."
(cd "$PIYA_ROOT/mcp-server" && npm install --silent)

mkdir -p "$(dirname "$MCP_JSON")"
node <<EOF
const fs = require('fs');
const path = '$MCP_JSON';
const entry = {
  command: 'node',
  args: ['$PIYA_ROOT/mcp-server/index.js'],
  env: {
    PIYA_BIN: '$PIYA_BIN',
    PIYA_PROJECT: '$PIYA_ROOT',
  },
};
let cfg = { mcpServers: {} };
if (fs.existsSync(path)) {
  cfg = JSON.parse(fs.readFileSync(path, 'utf8'));
  cfg.mcpServers ||= {};
}
cfg.mcpServers.piya = entry;
fs.writeFileSync(path, JSON.stringify(cfg, null, 2) + '\n');
console.log('Updated', path);
EOF

echo "==> Installing OpenClaw skill..."
mkdir -p "$(dirname "$SKILL_DST")"
rm -rf "$SKILL_DST"
cp -R "$SKILL_SRC" "$SKILL_DST"

OPENCLAW_JSON="$HOME/.openclaw/openclaw.json"
if [[ -f "$OPENCLAW_JSON" ]]; then
  node <<EOF
const fs = require('fs');
const path = '$OPENCLAW_JSON';
const cfg = JSON.parse(fs.readFileSync(path, 'utf8'));
cfg.skills ||= {};
cfg.skills.entries ||= {};
cfg.skills.entries.piya = { enabled: true };
fs.writeFileSync(path, JSON.stringify(cfg, null, 2) + '\n');
console.log('Enabled piya skill in openclaw.json');
EOF
fi

echo ""
echo "Done. Next steps:"
echo "  1. Restart Cursor (or reload MCP) so 'piya' tools appear"
echo "  2. Restart OpenClaw gateway if running"
echo "  3. In OpenClaw chat: delegate with piya_assign MCP tool"
echo ""
echo "Skill installed: $SKILL_DST"
echo "Binary: $PIYA_BIN"
