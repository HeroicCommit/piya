# Piya SwiftUI (macOS)

Native **SwiftUI** shell for Piya — menu bar + **Hummingbird** floating panel.  
Rust core (`libpiya.dylib`) is called via **UniFFI**.

## Build

From repo root:

```bash
./scripts/build-swift-app.sh
open dist/PiyaSwift.app
```

Requires Xcode / Swift 5.10+ and Rust toolchain.

## What’s included (Phase 0–1)

| Piece | Status |
|-------|--------|
| UniFFI bridge (`piya_chat`, MCP, queue) | ✅ |
| Menu bar app (`MenuBarExtra`) | ✅ |
| Hummingbird panel (`NSPanel`, `.ultraThinMaterial`) | ✅ |
| MCP Settings (native Form) | ✅ |
| Global hotkey everywhere | 🔜 (needs Accessibility permission) |

## Menu

- **Ask Piya… (Hummingbird)** — floating chat panel
- **MCP Settings…** — System Settings window
- **Quit Piya**

Shortcut when Piya is focused: **⌃⌘Space**

## Architecture

```
macos/PiyaApp/Sources/PiyaApp/   SwiftUI views + Hummingbird panel
macos/PiyaApp/Generated/         UniFFI output (regenerated on build)
src/ffi.rs                       Rust exports
src/chat_service.rs              Shared chat logic (wry + Swift)
```

## Next steps

1. Global hotkey (Carbon / `MASShortcut`) with Accessibility
2. Streaming tokens into SwiftUI
3. Tool-call activity chips during MCP
4. Retire `tao/wry` chat-panel once feature parity
5. Xcode project + TestFlight (optional)

## Data

Same as CLI: `~/Library/Application Support/Piya/data/`  
MCP config: `data/mcp.json`
