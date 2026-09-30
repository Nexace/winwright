# WINWRIGHT — Windows Semantic Automation Runtime
## Complete Build Specification for Claude Code

**Working name:** Winwright  
**Target OS:** Windows 11 first, Windows 10 only if compatibility is easy  
**Primary language/runtime:** Rust (stable, edition 2024) + Tokio + windows-rs  
**Primary interface:** MCP server + local CLI + loopback API  
**Goal:** Build the Windows equivalent of “Playwright/Playwriter for the entire desktop”.

---

# 1. Product Definition

Winwright is a local Windows automation runtime that gives an AI model a structured, semantic view of the user’s desktop and lets it operate Windows applications through high-level actions.

It should behave conceptually like Playwright:

- Playwright sees the browser DOM.
- Winwright sees the Windows UI Automation tree.
- Playwright locates elements by role, name, text, test id, etc.
- Winwright locates elements by control type, accessible name, AutomationId, framework, class name, text, window, and hierarchy.
- Playwright invokes DOM-level actions before resorting to raw pointer events.
- Winwright invokes UI Automation control patterns before resorting to SendInput.
- Playwright waits for elements/state.
- Winwright waits for UI Automation elements, windows, text, properties, events, and visual states.
- Playwright can screenshot.
- Winwright can screenshot the desktop, monitor, window, or bounding rectangle.
- Playwright has codegen.
- Winwright should eventually record desktop actions and generate reusable automation scripts.

This is **not** primarily a mouse macro recorder.

The desired interaction is:

```text
desktop.snapshot()

desktop.get_by_role(
    role="Button",
    name="Export"
).click()

desktop.get_by_label("File name").fill("final.png")

desktop.wait_for(
    text="Export complete",
    state="visible"
)
```

Coordinate clicking is a fallback, not the primary interface.

---

# 2. Product Experience

The intended user experience is inspired by the useful interaction ideas of products like HeyClicky:

1. A global hotkey summons the assistant.
2. The assistant can inspect what the user is looking at.
3. The assistant can explain what to do.
4. The assistant can point to UI elements using an overlay.
5. In agent mode, it can act on the computer.
6. It can execute multi-step workflows across applications.
7. It works locally and does not require application-specific plugins for basic UI control.

Do not copy third-party source code, proprietary assets, branding, character designs, sounds, or exact UI.

The implementation must be original.

---

# 3. Core Design Principle

Use the strongest control mechanism available in this order:

## Level 1 — Application/API operations

Prefer structured local APIs when possible.

Examples:

- filesystem APIs
- process APIs
- PowerShell
- application CLI commands
- Git
- Windows shell APIs
- COM APIs
- application-specific APIs explicitly supported by Winwright

These are faster and more reliable than UI interaction.

## Level 2 — Browser automation

If the active task is in a browser:

- use Playwright/CDP for webpage content where available
- keep Winwright responsible for the browser window itself, downloads, dialogs, file pickers, browser chrome, permission popups, and cross-app transitions

Do not replace Playwright with screenshot clicking inside webpages if DOM automation is available.

## Level 3 — Windows UI Automation

This is the primary desktop automation mechanism.

Use UI Automation to:

- inspect elements
- find controls
- read properties
- invoke controls
- set values
- select items
- toggle controls
- expand/collapse
- scroll
- read text
- manage windows

## Level 4 — Native pointer/keyboard input

Use Windows `SendInput` when:

- UI Automation does not expose the required action
- the application contains custom-rendered controls
- a UIA provider is incomplete
- drag/drop requires pointer interaction
- a canvas/editor needs direct interaction

## Level 5 — Vision

Use screenshots + image/vision analysis only when semantic automation cannot identify the intended target.

Vision may return:

- bounding rectangles
- detected text
- visual targets
- relative spatial descriptions

The final physical interaction still goes through Winwright’s input system.

---

# 4. Why This Architecture

A screenshot-only agent is:

- token-heavy
- slower
- fragile under scaling
- fragile when windows move
- fragile when themes change
- difficult to verify
- poor at text extraction
- poor at invisible/off-screen state

Windows UI Automation exposes a structured element tree with roles, names, states, properties, and supported operations.

Winwright should convert that tree into a compact representation designed specifically for LLM use.

---

# 5. Technical Stack

Use stable Rust (edition 2024), a Cargo workspace, and the `x86_64-pc-windows-msvc` target. Pin the tested toolchain in `rust-toolchain.toml` and commit `Cargo.lock`. Add ARM64 only after native compatibility tests. Tokio runs async orchestration, channels, deadlines, and transport servers; `windows-rs` provides Windows COM/WinRT/Win32 bindings. Use released compatible versions of `rmcp` for MCP, `axum` for the local API, `serde`/`serde_json` and compatible `schemars` for contracts, `thiserror` for typed errors, `tracing` for logs, and `clap` for CLI. Keep dependencies feature-scoped and add packages only when their phase needs them. Rust may lower idle overhead, but UIA providers and app response dominate action latency.

## Raw UI Automation COM

Create `CUIAutomation` through windows-rs and use `IUIAutomation`, `IUIAutomationElement`, conditions, tree walkers, cache requests, event handlers, and the appropriate control patterns. There is no FlaUI layer. Keep COM objects, HWNDs, and binding-specific types private to Windows adapters. Use feature-scoped `windows` bindings for this binary workspace, focused crates when useful, or private generated bindings for reusable libraries. See [windows-rs guidance](https://github.com/microsoft/windows-rs).

All UIA calls run on one long-lived dedicated OS thread initialized with `CoInitializeEx(COINIT_MULTITHREADED)`. It owns no windows; it creates/releases elements and adds/removes event handlers on that thread. Microsoft gives this guidance in [UI Automation threading](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-threading). Tokio sends owned commands through a bounded channel and receives owned DTOs via oneshot replies. No live COM interface crosses into arbitrary Tokio tasks. Keep event callbacks short and forward owned metadata without blocking. Balance COM initialization/uninitialization through an ownership guard; confine unsafe HRESULT/BSTR/VARIANT/SAFEARRAY interop to small documented platform functions.

```text
rmcp / axum / CLI / optional Tauri panel
             |
      Tokio core + policy
             |
 bounded commands and replies
             |
 dedicated MTA UIA worker
             |
 raw UI Automation COM
```

A timeout cannot interrupt a synchronous provider call or prove that an action did not execute. Check cancellation before dispatch and between calls; return an unknown-outcome error where appropriate and do not blindly retry. Isolate a repeatedly hanging provider in a supervised helper process if necessary; restarting it invalidates its element-handle epoch.

## MCP and API

Use the [official rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) on Tokio for stdio MCP and optional Streamable HTTP MCP. Enable the pinned release's server and transport features. Mount rmcp's HTTP service on an [axum](https://docs.rs/axum/latest/axum/) router at `/mcp`, bound to `127.0.0.1`. Tool handlers call the same core as CLI/API. Reserve stdout for MCP frames in stdio mode; logs go to stderr. Verify what transport each target AI client supports.

## Desktop shell and process model

Use Tauri only for tray, settings, inspector, assistant panel, and permission prompts where it helps. The engine, CLI, and MCP server run without WebView2. Native Win32 no-activate overlay windows render highlights; a native message loop can host hotkeys. Keep UI code out of the automation core. One per-user authority runs in the interactive desktop session; CLI/MCP/UI connect through same-process handles or current-user ACL-restricted named pipes. Optional HTTP/WebSocket binds to loopback. Coordinate a cross-process action lease so separate engine instances cannot inject conflicting input. Do not use a Session 0 service for interactive automation.

---

# 6. Repository Structure

Preserve the separation of contracts, core, Windows adapters, safety, transports, UI, and tests as Cargo packages. Add packages as phases need them; do not create empty scaffolding.

```text
winwright/
├─ Cargo.toml                  # workspace members and shared settings
├─ Cargo.lock                  # committed application lockfile
├─ rust-toolchain.toml         # tested toolchain
├─ README.md
├─ SECURITY.md
├─ CONTRIBUTING.md
├─ LICENSE
├─ docs/
│  ├─ architecture.md
│  ├─ tool-api.md
│  ├─ locator-model.md
│  ├─ security-model.md
│  ├─ permissions.md
│  ├─ troubleshooting.md
│  ├─ app-compatibility.md
│  └─ adr/
│     ├─ 0001-rust-runtime.md
│     ├─ 0002-raw-uia-mta-worker.md
│     ├─ 0003-rmcp-axum.md
│     └─ 0004-reference-snapshots.md
├─ crates/
│  ├─ winwright-contracts/     # elements, snapshots, locators, actions,
│  │                           # events, vision, windows, security DTOs
│  ├─ winwright-core/          # runtime, sessions, registry, locators,
│  │                           # actions, waits, snapshots, diagnostics
│  ├─ winwright-uia/           # COM worker, patterns, tree, events, cache
│  ├─ winwright-win32/         # windows, processes, displays, DPI, shell
│  ├─ winwright-input/         # keyboard, pointer, SendInput, drag, clipboard
│  ├─ winwright-capture/       # WGC, D3D, monitor/window/region, encoding
│  ├─ winwright-browser/       # optional Playwright/CDP handoff
│  ├─ winwright-files/         # filesystem, known folders, safe operations
│  ├─ winwright-shell/         # typed process/PowerShell execution
│  ├─ winwright-vision/        # local/remote adapters, OCR, grounding
│  ├─ winwright-overlay/       # native highlights, arrows, labels
│  ├─ winwright-security/      # policy, redaction, confirmations, audit
│  ├─ winwright-mcp/           # rmcp tools, resources, transports
│  ├─ winwright-api/           # axum routes, WebSocket, authentication
│  ├─ winwright-cli/           # winwright.exe, serve/mcp entry points
│  ├─ winwright-recorder/      # hooks, UIA events, codegen
│  └─ winwright-test-support/  # fixture launch and scenarios
├─ apps/winwright-desktop/     # optional Tauri 2 UI shell
│  ├─ src/
│  └─ src-tauri/
├─ fixtures/
│  ├─ test-win32-app/          # Rust standard-control UIA fixture
│  ├─ test-custom-canvas/      # Rust visual-only fixture
│  ├─ test-webview-app/
│  ├─ external-wpf/            # compatibility fixtures only
│  ├─ external-winforms/
│  └─ external-winui/
├─ tests/                     # scenario data wired to a Cargo test package
├─ scripts/
│  ├─ build.ps1
│  ├─ test.ps1
│  ├─ package.ps1
│  └─ install-dev.ps1
└─ examples/
   ├─ mcp/
   ├─ cli/
   ├─ rust/
   └─ workflows/
```

Implemented packages have their own `Cargo.toml` and `src/lib.rs` or `src/main.rs`. Cargo package-level `tests/` contain integration tests; root `tests/` in a virtual workspace needs explicit wiring. Start with contracts, core, UIA, Win32, security, and CLI. Keep small locator/snapshot/wait modules in core until a split has a concrete benefit. Dependency direction is transport/shell -> core -> contracts/backend traits -> platform adapters. No raw COM or Tauri types escape into contracts.

---

# 7. The LLM-Facing Mental Model

Winwright should expose four key concepts:

```text
Desktop
  └─ Application
       └─ Window
            └─ Element
```

An element must be addressable through:

- temporary snapshot reference
- stable-ish locator
- native runtime ID if available

Example:

```json
{
  "ref": "e42",
  "role": "Button",
  "name": "Save",
  "automationId": "SaveButton",
  "className": "Button",
  "framework": "WPF",
  "enabled": true,
  "visible": true,
  "focused": false,
  "bounds": [1240, 820, 1324, 858],
  "patterns": ["Invoke"],
  "path": "Window[Notepad] > Pane[Editor] > Button[Save]"
}
```

Never expose raw native objects to the AI.

---

# 8. Snapshot System

This is one of the most important components.

## Goal

Convert an enormous UI Automation tree into a compact, model-friendly snapshot.

Default snapshot:

- active window only
- interactive controls
- meaningful text controls
- hierarchy retained
- decorative objects omitted
- off-screen elements omitted unless requested
- sensitive values redacted
- each relevant node gets a short reference

Example:

```text
WINDOW "Save As" [w1]
  TEXT "Save in:" [e1]
  COMBOBOX "Documents" [e2]
  TREE "Folders" [e3]
  EDIT "File name:" value="" [e4]
  COMBOBOX "Save as type:" value="PNG" [e5]
  BUTTON "Save" [e6]
  BUTTON "Cancel" [e7]
```

The model can then call:

```text
desktop.fill(ref="e4", text="image.png")
desktop.click(ref="e6")
```

## Snapshot modes

```text
active-window
all-windows
app
window
subtree
interactive
text
raw-debug
```

## Snapshot options

```json
{
  "interactiveOnly": true,
  "includeText": true,
  "includeBounds": false,
  "includePatterns": false,
  "maxDepth": 12,
  "maxNodes": 500,
  "includeOffscreen": false
}
```

---

# 9. Element Reference System

Playwright uses locators that can resolve repeatedly.

Winwright must not rely purely on stale UIA objects.

Each snapshot creates references:

```text
e1
e2
e3
...
```

Store internally:

```text
ref
runtimeId
processId
windowHandle
automationId
name
controlType
className
frameworkId
ancestor fingerprint
bounding rectangle
snapshot generation
```

When an action uses `e42`:

1. Try exact runtime element.
2. Validate that it still matches its fingerprint.
3. If stale, re-resolve using locator data.
4. If multiple matches appear, return ambiguity rather than guessing.

Default reference TTL:

```text
30 seconds
```

but allow continued use if validation succeeds.

Refs belong to a caller session and snapshot generation, not a global counter. A CLI `snapshot` followed by `click e42` needs a persistent per-user engine; separate short-lived processes cannot safely recreate the reference. Provide `winwright serve` and `--session <id>` or an equivalent current-user context. Reject cross-session/expired/reused refs, validate process lifetime and HWND reuse, and invalidate all element slots after worker restart.

---

# 10. Locator Engine

This is the desktop equivalent of Playwright locators.

Support:

## Role

```text
get_by_role("Button")
get_by_role("Button", name="Save")
```

Map role to UIA ControlType.

## Accessible name

```text
get_by_name("Export")
```

## Text

```text
get_by_text("Export complete")
```

Support:

```text
exact
contains
regex
caseSensitive
```

## AutomationId

```text
get_by_automation_id("btnSave")
```

## Label

```text
get_by_label("File name")
```

Implement label inference using:

- LabeledBy
- sibling text
- parent grouping
- common form geometry only as fallback

## Class

```text
get_by_class("Button")
```

## Framework

```text
framework = "WPF"
framework = "Win32"
framework = "WinForm"
framework = "XAML"
framework = "Chrome"
```

## Relative locator

```text
locator(role="Edit")
  .near(text="File name")
```

## Hierarchical

```text
window("Settings")
  .locator(role="Tab", name="System")
  .locator(role="Button", name="Display")
```

## Index

Allow `.nth()` only as an explicit fallback.

Never default to positional selection when a semantic locator exists.

---

# 11. UIA Action Mapping

Map high-level actions to UI Automation control patterns.

## click()

Preferred strategy:

1. InvokePattern
2. SelectionItemPattern
3. TogglePattern where semantically appropriate
4. clickable point
5. center of bounding rectangle + SendInput

## fill(text)

Preferred strategy:

1. ValuePattern.SetValue when enabled and writable
2. an explicitly supported app or legacy accessibility setter
3. focus + Ctrl+A + SendInput Unicode text
4. clipboard paste only when policy permits

Do not expose secrets through clipboard unless policy explicitly permits it.

## select(option)

Preferred:

1. ExpandCollapse
2. SelectionItem
3. Value
4. keyboard navigation

## check()/uncheck()

Use TogglePattern.

## expand()/collapse()

Use ExpandCollapsePattern.

## scroll()

Use ScrollPattern / ScrollItemPattern.

Fallback:

- wheel input
- keyboard page navigation

## drag_to()

Preferred:

1. application-specific semantic drag when available
2. validated SendInput pointer path
3. UIA Drag/DropTarget metadata/events for observation and verification

## focus()

Use UIA SetFocus before raw clicking.

## read_text()

Use:

1. TextPattern
2. ValuePattern
3. Name
4. Legacy accessibility fallback

`TextPattern` and `TextEdit` do not provide a general text setter; use writable `ValuePattern`, a supported app/legacy setter, or controlled keyboard input ([Microsoft TextPattern overview](https://learn.microsoft.com/en-us/dotnet/framework/ui-automation/ui-automation-textpattern-overview)). UIA Drag/DropTarget supply observation data, not a universal drag command; do not invent `DragTo` ([native DragPattern](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nn-uiautomationclient-iuiautomationdragpattern)). Read ToggleState before check/uncheck. Never retry physically if a semantic action may already have succeeded.

---

# 12. MCP Tool Surface

The MCP API should be small enough that models understand it.

Do not expose 100 tiny tools initially.

Start with approximately 20.

## Observation

```text
desktop_snapshot
desktop_screenshot
desktop_windows
desktop_active_window
desktop_inspect
```

## Finding

```text
desktop_find
desktop_find_all
```

## Interaction

```text
desktop_click
desktop_fill
desktop_press
desktop_hotkey
desktop_select
desktop_toggle
desktop_scroll
desktop_drag
desktop_focus
```

## Window control

```text
window_focus
window_move
window_resize
window_minimize
window_maximize
window_close
```

## Waiting

```text
desktop_wait_for
```

## System

```text
app_launch
process_list
filesystem_operation
shell_execute
```

Keep risky system operations separately permissioned.

---

# 13. Preferred MCP Tool Schemas

## desktop_snapshot

Input:

```json
{
  "target": "active",
  "interactiveOnly": true,
  "includeText": true,
  "maxDepth": 12,
  "maxNodes": 500
}
```

Output:

```json
{
  "generation": "s_104",
  "activeWindow": {
    "ref": "w1",
    "title": "Untitled - Notepad",
    "process": "notepad.exe"
  },
  "tree": "...compact textual tree..."
}
```

## desktop_find

Input:

```json
{
  "window": "active",
  "role": "Button",
  "name": "Save",
  "text": null,
  "automationId": null,
  "exact": true,
  "visibleOnly": true
}
```

Output:

```json
{
  "count": 1,
  "matches": [
    {
      "ref": "e6",
      "role": "Button",
      "name": "Save"
    }
  ]
}
```

## desktop_click

Input:

```json
{
  "ref": "e6",
  "button": "left",
  "clickCount": 1,
  "forcePhysical": false
}
```

Output:

```json
{
  "success": true,
  "method": "InvokePattern",
  "durationMs": 18
}
```

## desktop_fill

```json
{
  "ref": "e4",
  "text": "report.docx",
  "clear": true
}
```

## desktop_wait_for

```json
{
  "role": "Text",
  "name": "Export complete",
  "state": "visible",
  "timeoutMs": 10000
}
```

Possible states:

```text
exists
missing
visible
hidden
enabled
disabled
focused
value
text
window-open
window-closed
```

---

# 14. Semantic Action Resolver

The AI should not need to know UIA pattern details.

Given:

```text
click e24
```

ActionEngine performs:

```text
resolve reference
validate element
check security policy
discover supported patterns
choose least invasive semantic method
execute
verify result
return method and state change
```

Every action returns:

```json
{
  "success": true,
  "method": "InvokePattern",
  "changed": true,
  "before": "...optional...",
  "after": "...optional...",
  "warnings": []
}
```

---

# 15. Wait Engine

Fixed sleeps should be discouraged.

Implement Playwright-like waits.

Examples:

```text
wait_for role=Dialog name="Save As"
wait_for text="Finished"
wait_for window="Photoshop"
wait_for ref=e10 state=enabled
wait_for ref=e10 state=missing
```

Implementation:

1. UIA event subscription when available
2. bounded polling fallback
3. process/window event fallback
4. timeout with useful diagnostics

Return why the wait failed.

Use Tokio deadlines and cancellation-aware waits around the UIA mailbox. Subscribe, check current state, await relevant events, and re-query the predicate; an event alone is not proof. Include bounded polling for missed events. Unsubscribe on the owning MTA worker even after cancellation.

---

# 16. Event System

Support internal events:

```text
WindowOpened
WindowClosed
FocusChanged
ElementAdded
ElementRemoved
PropertyChanged
StructureChanged
TextChanged
SelectionChanged
ProcessStarted
ProcessExited
```

Expose optional MCP resource/event streaming later.

Use events internally immediately for:

- waits
- recorder
- context refresh
- stale reference detection

---

# 17. Window Management

Implement reliable APIs for:

```text
list windows
find window by title/process
focus
foreground
move
resize
minimize
maximize
restore
close
get bounds
get process
get monitor
```

Handle:

- multi-monitor
- DPI scaling
- borderless windows
- hidden windows
- owned dialogs
- modal dialogs
- always-on-top windows

---

# 18. Screen Capture

Implement:

```text
capture desktop
capture monitor
capture window
capture element
capture region
```

Use Windows.Graphics.Capture for modern window/display capture.

For element capture:

1. obtain bounds from UIA
2. capture owning window/display
3. transform physical desktop bounds into capture-local pixels and crop (no double scaling)

Output formats:

```text
PNG default
JPEG optional
WebP optional if dependency is justified
```

Screenshot calls should optionally return:

```text
image path
width
height
DPI
monitor
window
cropped bounds
```

Do not continually capture the screen unless an active task explicitly requests observation.

Implement Windows.Graphics.Capture through windows-rs WinRT with explicit D3D/frame-pool ownership and cleanup. Feature-detect capture and return typed errors for protected, minimized, closed, or unavailable targets. Compose monitors for whole-desktop capture if needed. Return owned bytes/metadata, not live graphics objects. Bound buffers, avoid persistence by default, and mask sensitive regions where feasible; refuse unsafe captures when secrets cannot be excluded.

---

# 19. Vision Fallback

Vision remains a fallback. The `VisionGrounder` Rust trait in Section 64 returns visual targets with label, confidence, bounds, and optional text. `desktop_click_visual` revalidates the capture/window/coordinates and delegates to physical input after policy. Allow local or provider-agnostic remote adapters; sending screen data off-device requires explicit awareness and permission.

---

# 20. Browser Handoff

Detect Chromium/Edge/Chrome/Firefox browser windows.

When task target is webpage content, expose a browser bridge.

Example:

```text
desktop sees Chrome
        ↓
browser bridge attaches
        ↓
Playwright handles page DOM
        ↓
Winwright handles native file picker
        ↓
Playwright resumes page automation
```

This is significantly stronger than trying to use UIA for every website.

Keep browser automation optional.

Use an optional version-pinned Node Playwright sidecar over structured local IPC, or a scoped CDP adapter with documented limits. CDP covers Chromium targets; Firefox needs the appropriate Playwright-managed path. Browser actions share session, policy, cancellation, and target identity with desktop actions. Missing sidecar returns a capability error rather than silently switching to screenshot clicks.

---

# 21. Overlay System

Create transparent topmost overlays that do not steal focus.

Features:

```text
highlight rectangle
animated border
arrow
label
step number
click marker
cursor spotlight
draw/annotation layer
```

API:

```text
overlay.highlight(ref="e12")
overlay.label(ref="e12", text="Click this")
overlay.arrow(ref="e12")
overlay.clear()
```

This supports tutorial/talk mode.

It also helps debugging.

Render overlays in Rust through no-activate native HWNDs on their own message-loop thread, separate from the MTA UIA worker. Implement click-through hit testing, DPI-aware positions, and cancellation cleanup. Tauri may host assistant/settings/inspector panels; overlays must not steal focus.

---

# 22. Global Hotkey

Desktop app should support configurable hotkey:

Default proposal:

```text
Ctrl + Space
```

Do not hardcode permanently.

Modes:

```text
Hold hotkey -> inspect/talk
Hotkey + A -> agent
Hotkey + Esc -> cancel current task
```

Provide an emergency stop hotkey that cannot be overridden by the model.

Example:

```text
Ctrl + Alt + Esc
```

Use `RegisterHotKey` on a native message loop for press-based hotkeys and emergency stop. Report conflicts and allow another binding. Hold/release mode may require a low-level hook; defer it until needed. The model cannot disable the emergency binding, which works even when Tauri closes.

---

# 23. Agent Session Model

Winwright itself should remain a tool runtime.

Do not tightly couple the automation engine to one planner.

Session:

```json
{
  "id": "sess_x",
  "startedAt": "...",
  "owner": "mcp",
  "activeTask": "...",
  "permissions": {},
  "snapshotGeneration": "...",
  "currentWindow": "...",
  "cancelled": false
}
```

The external model:

1. observes
2. reasons
3. calls action
4. receives result
5. observes only when necessary

This is much cheaper than sending full screenshots every step.

---

# 24. Safety / Permission System

This is required, not optional.

## Permission levels

### Safe

No confirmation:

```text
inspect UI
read visible text
list windows
take on-demand screenshot
focus window
scroll
navigate menus
type into ordinary non-sensitive fields
```

### Confirm

Require user confirmation by default:

```text
send message/email
submit form
publish/post
delete file
delete cloud content
move to recycle bin
install/uninstall software
execute unsigned downloaded program
change security settings
make purchase
confirm order
financial transaction
change password
grant permissions
run process elevated
terminate unrelated process
```

### Block by default

```text
read password field
extract credential store secrets
disable antivirus/security
bypass UAC
silent privilege escalation
capture authentication secrets
copy password-field contents
```

Users may explicitly configure additional permissions, but Winwright should ship conservative defaults.

---

# 25. Sensitive Field Detection

Inspect UIA properties such as password/sensitive indicators where available.

Redact snapshot:

```text
EDIT "Password" value="[REDACTED]" sensitive=true
```

Never return the password value to the model.

If the model requests:

```text
read e42
```

return:

```json
{
  "error": "SENSITIVE_FIELD",
  "message": "Reading password fields is blocked."
}
```

Typing a user-provided secret may be supported through a secret reference system later, without revealing the secret to the model.

---

# 26. UAC / Integrity Boundaries

Winwright must not pretend it can interact with everything.

Windows input injection is constrained by process integrity levels.

Default process runs non-elevated.

If attempting to automate an elevated application:

- detect integrity mismatch
- return a clear permission error
- do not silently restart elevated
- allow the user to explicitly launch a separate elevated helper

Keep elevated helper:

```text
minimal
separately permissioned
named-pipe controlled
strict command allowlist
```

---

# 27. Audit Log

Every action should produce a structured audit event.

Example:

```json
{
  "timestamp": "...",
  "session": "sess_123",
  "tool": "desktop_click",
  "target": {
    "process": "notepad.exe",
    "window": "Save As",
    "role": "Button",
    "name": "Save"
  },
  "method": "InvokePattern",
  "result": "success",
  "confirmation": false
}
```

Sensitive text must be redacted.

Allow:

```text
disable normal telemetry
local audit only
clear logs
export logs
```

Default should be local-only.

---

# 28. CLI

CLI gives developers a debugging surface independent of AI.

Examples:

```powershell
winwright windows
winwright snapshot
winwright snapshot --window "Notepad"
winwright find --role Button --name Save
winwright click e14
winwright fill e7 "hello"
winwright screenshot --active
winwright inspect --under-cursor
winwright watch
winwright mcp
```

Add JSON output:

```powershell
winwright snapshot --json
```

This is important for testing.

Back commands with one per-user engine so refs survive process boundaries:

```powershell
winwright serve
winwright snapshot --session demo
winwright find --session demo --role Button --name Save
winwright click --session demo e14
```

The engine owns identity/permissions; the CLI session name is only a selector. `winwright mcp` reserves stdout for rmcp and attaches to the same authority. Include session/generation metadata when needed.

---

# 29. Inspector

Build an inspector similar in spirit to Playwright Inspector.

UI:

Left:

```text
UIA tree
```

Right:

```text
Name
ControlType
AutomationId
ClassName
FrameworkId
ProcessId
BoundingRectangle
Enabled
Offscreen
Focused
SupportedPatterns
RuntimeId
```

Features:

```text
pick element under cursor
highlight selected element
copy locator
copy reference
test click
test fill
view ancestors
view children
view raw UIA values
```

This should be implemented before advanced AI UX.

---

# 30. Recorder / Codegen

Later phase.

Record:

```text
mouse clicks
keyboard activity
focused element
UIA element under action
window changes
text entry
selection changes
```

Convert raw interaction:

```text
mouse click (842, 517)
```

into:

```text
desktop.get_by_role("Button", name="Export").click()
```

If semantic resolution is weak:

```text
desktop.click_at(842, 517)
```

Mark fallback actions visibly.

Generated formats:

```text
Winwright Rust client example
Winwright JSON workflow
MCP replay plan
```

Generate compilable Rust SDK examples and versioned JSON/YAML workflows. Exclude password entry, clipboard secrets, and unrestricted keystroke histories from recording; mark every physical fallback.

---

# 31. Workflow File Format

Support reusable deterministic workflows.

Example:

```yaml
version: 1

name: save-notepad-file

steps:
  - action: window.focus
    title: "Notepad"

  - action: desktop.hotkey
    keys: ["CTRL", "SHIFT", "S"]

  - wait:
      role: "Window"
      name: "Save As"

  - action: desktop.fill
    locator:
      role: "Edit"
      label: "File name"
    text: "notes.txt"

  - action: desktop.click
    locator:
      role: "Button"
      name: "Save"
```

The workflow runner should use the same locator/action engine as MCP.

---

# 32. Error Model

Never return vague:

```text
failed
```

Use typed errors:

```text
ELEMENT_NOT_FOUND
ELEMENT_AMBIGUOUS
ELEMENT_STALE
WINDOW_NOT_FOUND
WINDOW_NOT_FOCUSED
UNSUPPORTED_PATTERN
ACTION_BLOCKED
CONFIRMATION_REQUIRED
UIPI_BLOCKED
TIMEOUT
PROCESS_EXITED
CAPTURE_FAILED
DPI_CONVERSION_FAILED
INPUT_FAILED
VISION_NO_MATCH
```

Include recovery hints.

Example:

```json
{
  "error": "ELEMENT_AMBIGUOUS",
  "message": "3 visible buttons named Save were found.",
  "matches": ["e12", "e19", "e22"],
  "hint": "Narrow by parent/window/AutomationId."
}
```

Use `thiserror` variants internally and stable serialized codes. Add `CANCELLED`, `DESKTOP_BUSY`, `BACKEND_UNAVAILABLE`, and `ACTION_OUTCOME_UNKNOWN`. A transport failure after dispatch may mean an action executed; do not retry automatically. Keep HRESULT diagnostics safe and redacted.

---

# 33. Verification After Actions

Do not assume an action worked.

Examples:

## Click

Possible verification:

- dialog opened
- element disappeared
- focus changed
- selection changed
- window changed

## Fill

Read ValuePattern back when safe.

## Toggle

Verify ToggleState.

## Window close

Verify HWND disappeared.

Tool output should differentiate:

```text
executed
verified
```

Example:

```json
{
  "executed": true,
  "verified": false,
  "warning": "Control was invoked but no observable state change was detected."
}
```

---

# 34. Performance Targets

MVP targets on ordinary hardware:

```text
active window snapshot: < 150 ms typical
element lookup: < 100 ms typical
semantic click: < 100 ms plus app response
window list: < 50 ms typical
screenshot active window: < 250 ms
MCP overhead: < 30 ms local
```

Do not block the UI thread with tree traversal.

Cache:

- process metadata
- window metadata
- static UIA properties
- supported pattern metadata cautiously

Do not cache live state indefinitely.

---

# 35. Token-Efficient Snapshot Rules

Default tree filtering:

Include:

```text
Window
Dialog
Menu
MenuItem
Button
Edit
ComboBox
CheckBox
RadioButton
Tab
TabItem
List
ListItem
Tree
TreeItem
DataGrid
Table
Link
Text where meaningful
Pane when named
custom controls when interactive
```

Usually omit:

```text
unnamed layout containers
decorative images
duplicate nested text
zero-sized elements
off-screen elements
empty panes
repeated internal Chrome accessibility wrappers where redundant
```

Truncate huge lists.

Example:

```text
LIST "Files" [e7] children=384 showing=20
```

Provide explicit request for more.

---

# 36. App Compatibility Strategy

Create an internal compatibility matrix.

Test at minimum:

```text
Windows Settings
File Explorer
Notepad
Calculator
Paint
Microsoft Edge
Google Chrome
Firefox
VS Code
Microsoft Office apps if installed
Adobe apps if installed
Electron apps
WPF apps
WinForms apps
WinUI apps
classic Win32 dialogs
Java/Qt apps where possible
```

For each:

```text
tree quality
locator quality
click
fill
scroll
text extraction
dialogs
menus
fallback required
known bugs
```

---

# 37. Special Handling for Custom-Rendered Apps

Some applications expose poor accessibility trees.

Fallback sequence:

```text
UIA
↓
Legacy accessibility if useful
↓
keyboard shortcuts
↓
window/client-coordinate heuristics
↓
vision
↓
physical click
```

Do not pretend all apps will be fully semantic.

Apps with canvases, games, remote desktops, video editors, 3D software, and custom GPU surfaces may require substantial vision/input fallback.

---

# 38. Photoshop / Premiere / Creative Apps

Treat creative apps as hybrid automation targets.

Use:

```text
menus and dialogs -> UIA
keyboard shortcuts -> hotkey engine
file operations -> filesystem
custom canvas -> vision/input
application scripting APIs -> optional adapters
```

Do not try to model an editing canvas entirely through UIA if the app does not expose semantic elements.

---

# 39. Process / Shell Tools

Expose conservative system tools.

## app_launch

```json
{
  "app": "notepad.exe",
  "args": []
}
```

## shell_execute

Default:

```text
disabled or confirmation required
```

Allow read-only/common commands via policy.

Support separate tool:

```text
powershell_execute
```

but make arbitrary shell execution a higher-risk capability.

Use std/Tokio process APIs with executable plus argument lists. PowerShell is an optional higher-risk adapter. Bound output/time, redact sensitive arguments and environment, and avoid string-built shell commands for ordinary file/process operations.

---

# 40. File Tools

Do not force the AI to manipulate Explorer visually for ordinary file operations.

Provide:

```text
file_list
file_read_metadata
file_copy
file_move
file_rename
file_delete
directory_create
file_search
```

Destructive actions respect confirmation policy.

Use Explorer UI automation only when the task specifically concerns what the user sees in Explorer.

---

# 41. Context Compression

Do not resend the entire desktop after every action.

Maintain:

```text
previous snapshot
new snapshot
diff
```

Return:

```text
added
removed
changed
focused
windowChanged
```

Example:

```text
DIFF s104 -> s105
+ WINDOW "Save As" [w2]
focus -> EDIT "File name" [e41]
```

This will materially reduce model context usage.

---

# 42. Snapshot Diff Engine

Compare:

```text
window identity
element fingerprints
tree structure
properties
focus
bounds
value
toggle state
selection
```

Use stable-ish signatures but avoid assuming native RuntimeId survives recreation.

---

# 43. Multi-Monitor and DPI

Represent physical desktop pixels (signed virtual-desktop coordinates), capture-local pixels/origin, window-client coordinates, UI-shell DIPs, monitor identity, and scale as distinct Rust types. UIA bounding rectangles/clickable points use physical screen coordinates; do not scale twice ([UIA scaling guidance](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-screenscaling)). Declare Per-Monitor V2 DPI awareness before HWND creation. For absolute SendInput normalize against the full virtual desktop with `MOUSEEVENTF_VIRTUALDESK` and `MOUSEEVENTF_ABSOLUTE`. Test 100–200% scales, mixed/portrait monitors, negative origins, and spanning windows.

---

# 44. Cancellation

Each long operation takes an `OperationContext` with CancellationToken, action epoch, and monotonic deadline. Stop cancels the session, invalidates queued actions, halts waits, hides overlays, and releases only synthesized buttons/modifiers through an independent cleanup path. Check cancellation before each input batch. A synchronous provider call cannot be guaranteed to stop instantly; report uncertainty and prevent subsequent actions.

---

# 45. Concurrency

One active session controls state-changing desktop actions, including semantic UIA operations. Other clients may inspect. Coordinate an action lease across CLI, MCP, API, UI shell, and helpers. Physical input holds an exclusive focus-plus-input lock; stop cleanup bypasses ordinary queues. Return DESKTOP_BUSY absent explicit takeover. Bound command queues and avoid callback/lock deadlocks.

---

# 46. Configuration

Example:

```json
{
  "server": {
    "mcpStdio": true,
    "http": true,
    "httpHost": "127.0.0.1",
    "httpPort": 32145
  },
  "automation": {
    "defaultTimeoutMs": 10000,
    "referenceTtlSeconds": 30,
    "maxSnapshotNodes": 500
  },
  "capture": {
    "onDemandOnly": true
  },
  "security": {
    "confirmationMode": "balanced",
    "blockPasswordRead": true,
    "allowShell": false
  },
  "overlay": {
    "enabled": true
  }
}
```

---

# 47. Local API

Use axum for versioned loopback endpoints: GET /v1/windows, /v1/snapshot; POST /v1/find, /v1/action/click, /fill, /press, /v1/wait, /v1/capture, /v1/session/cancel. Protect /v1 and /mcp with random token, Host/Origin checks, disabled CORS by default, and request/body/queue limits. Share DTOs, policy, and action engine with MCP/CLI. A model-supplied approval flag never counts as human confirmation.

---

# 48. SDK

A later Rust client may expose `desktop.get_by_role(ControlRole::Button).name("Save").click().await?` backed by the same locator/action service. TypeScript/Python can call the versioned API later. A C ABI/CDylib comes only when a real native consumer needs it. SDKs are outside MVP.

---

# 49. Testing Philosophy

Tests should cover semantics, not coordinates.

Bad:

```text
click x=421 y=912
```

Good:

```text
get button Save
invoke it
assert Save As window disappeared
```

---

# 50. Fixture Applications

Build controlled Rust Win32 fixtures with accessible buttons, edit, checkbox, radio, tabs, tree/list/grid, menu/dialog, disabled, scrolling, dynamic, password, delayed and recreated controls. Add a deliberately inaccessible custom canvas and a WebView fixture. WPF/WinForms/WinUI compatibility fixtures may use their own build tooling on separate runners; they do not make Winwright itself depend on .NET.

---

# 51. Unit Tests

Test:

```text
locator parsing
locator ranking
element fingerprinting
snapshot compression
diffing
typed errors
permission policy
sensitive field redaction
action strategy selection
coordinate conversion
retry logic
```

Use `#[test]` and `#[tokio::test]` with fake backends and paused time for pure logic. Test worker epoch invalidation, cross-session refs, action uncertainty, confirmation binding, event overflow, cancellation, and synthesized-key cleanup. Round-trip serde DTOs and validate schemas. Keep real-desktop tests separate.

---

# 52. Integration Tests

Examples:

```text
launch fixture
snapshot
locate button
click
assert text changes

open dialog
locate edit by label
fill
save
assert file exists

toggle checkbox
assert toggle state

scroll virtualized list
locate newly visible item

close/recreate control
use stale ref
assert re-resolution works
```

Cargo package-level `tests/` targets launch controlled fixtures. Gate physical input/capture/foreground tests behind explicit opt-in, serialize with `--test-threads=1`, and run on a dedicated unlocked Windows VM. Use temporary folders and owned processes, never arbitrary user apps. Test UIA event subscription ownership, stale handles, mixed-DPI capture alignment, provider hangs, emergency stop cleanup, and password redaction across snapshots, logs, audit, recorder, and screenshots. Test rmcp framing and axum authentication/routes with a mock core.

---

# 53. End-to-End Scenarios

Before calling MVP complete:

## Notepad

```text
launch
type text
Save As
enter filename
save
close
```

## File Explorer

```text
open folder
search
select file
rename
```

## Windows Settings

```text
open
navigate using semantic locators
read state
do not modify sensitive settings by default
```

## Browser + native dialog

```text
Playwright opens upload control
Winwright handles native file dialog if required
browser resumes
```

---

# 54. Logging

Use `tracing`/`tracing-subscriber` with session/request/snapshot/worker-epoch spans. Report typed failures and safe HRESULT categories. Log to stderr in stdio MCP mode. Never log secret-bearing fill text or screenshot data by default; log safe summaries such as `fill Edit[Password] chars=16 sensitive=true`.

---

# 55. Diagnostics Bundle

`winwright diagnostics export` includes version/commit, OS build, architecture, build-time Rust toolchain, locked backend versions, display/DPI, worker health, compatibility results, redacted errors and config. Exclude tokens, credentials, field text, and screenshots unless explicitly selected. Release binaries require no installed Rust toolchain.

---

# 56. Build and Packaging

Use Windows with a tested stable Rust MSVC toolchain, Microsoft C++ Build Tools, and Windows SDK. Node/frontend tooling is only for optional Tauri or Playwright sidecars. Engine builds do not require .NET or WebView2. Pin released compatible dependencies and commit `Cargo.lock`.

Once the named packages exist, run from the workspace root:

```powershell
cargo run -p winwright-cli -- windows
cargo run -p winwright-cli -- snapshot
cargo run -p winwright-cli -- mcp
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build -p winwright-cli --release --locked --target x86_64-pc-windows-msvc
```

Name the CLI binary `winwright`. Set workspace default members so normal engine work does not require the optional UI or external fixtures. Unit, schema, and mock-transport tests run in ordinary CI. UIA, WGC, SendInput, hotkey, and overlay tests are opt-in, serialized, and run on a dedicated unlocked interactive Windows desktop. Build the optional Tauri shell from `apps/winwright-desktop` using its pinned CLI. Document its WebView2 policy and MSI/NSIS packaging using the [Tauri installer guide](https://v2.tauri.app/distribute/windows-installer/).

Release a native x64 `winwright.exe` in a portable ZIP first, with checksums, licenses, sample config, and MCP launch instructions. Optional `winwright-desktop.exe`, sidecars, and an explicitly consented helper have separate versioned assets. Add signed installer and ARM64 only after compatibility testing. Verify CRT/DLL linkage and clean-machine behavior before claiming the package is self-contained. Run fmt, Clippy, relevant tests, release build, dependency/license checks, and a clean Windows VM smoke test. Do not optimize packaging before core automation is reliable.

---

# 57. Phase Plan

## Phase 0 — Repository + contracts

Deliver:

```text
Cargo workspace
Rust packages
owned contracts and backend traits
logging
configuration
tests bootstrapped
```

Acceptance:

```text
Cargo workspace builds on Windows MSVC
cargo test runs
CLI prints version
```

---

## Phase 1 — Window + UIA observation

Implement:

```text
window list
active window
UIA tree
compact snapshot
inspect element
element refs
```

CLI:

```text
winwright windows
winwright snapshot
winwright inspect
```

Acceptance:

- Notepad tree readable.
- Settings tree readable.
- File Explorer tree readable.
- interactive controls get refs.

This phase is the foundation.

---

## Phase 2 — Semantic locators

Implement:

```text
role
name
text
AutomationId
label
class
framework
ancestor
nth
```

Acceptance:

```text
winwright find --role Button --name Save
```

resolves expected fixture controls reliably.

---

## Phase 3 — Semantic actions

Implement:

```text
click
fill
focus
select
toggle
expand/collapse
scroll
press
hotkey
```

Pattern-first.

Acceptance:

- controlled fixture workflows work with no coordinate clicking where UIA is available.

---

## Phase 4 — Waits + events + verification

Implement:

```text
wait_for
UIA event subscriptions
action verification
snapshot diff
```

Acceptance:

- no fixed sleeps needed in normal fixture workflows.

---

## Phase 5 — MCP

Implement the official Rust rmcp MCP server on Tokio.

Expose:

```text
snapshot
find
click
fill
press
wait
windows
focus
screenshot
```

Acceptance:

- an MCP-capable model can operate Notepad using semantic tools.

Do not add agent planning inside Winwright yet.

---

## Phase 6 — Capture + overlays

Implement:

```text
window capture
screen capture
element crop
highlight overlay
arrow
label
```

Acceptance:

- selected UIA element can be highlighted accurately at mixed DPI.

---

## Phase 7 — Physical fallback

Implement:

```text
SendInput keyboard
SendInput pointer
click point
drag
wheel
coordinate normalization
```

Acceptance:

- custom canvas fixture can be physically controlled.

---

## Phase 8 — Security

Implement:

```text
permission engine
confirmations
password redaction
audit
cancel
exclusive action lock
elevated-app detection
```

Acceptance:

- password values are never returned.
- blocked/confirm actions behave correctly.
- emergency stop always halts queued work.

---

## Phase 9 — Browser bridge

Integrate Playwright optionally.

Acceptance:

- semantic browser DOM automation and Winwright native dialog automation can coexist in one workflow.

---

## Phase 10 — Vision fallback

Implement the Rust VisionGrounder trait.

Start provider-agnostic.

Acceptance:

- visual-only target can be found from screenshot and clicked.
- model receives explicit indicator that vision fallback was used.

---

## Phase 11 — Desktop UX

Use optional Tauri for settings, inspector, assistant panel, and permission UI; keep native overlays/hotkeys in Rust.

Implement:

```text
tray icon
hotkey
small assistant panel
talk/agent modes
permissions UI
session indicator
stop button
```

Keep UX separate from automation core.

---

## Phase 12 — Recorder/codegen

Record user actions and generate semantic workflows.

Acceptance:

- a basic Notepad save flow recorded by a human can replay semantically.

## Rust sequencing notes

Keep all 13 phases and acceptance checks above. Phase 0 also establishes the MTA worker boundary, toolchain/lockfile, basic action lease, cancellation, redaction, and default-deny for risky capabilities. Phase 1 adds persistent session/reference authority. Phase 4 checks subscription lifetimes and uncertain outcomes. Phase 5 uses rmcp; Phase 6 uses windows-rs capture/native overlays; Phase 11 may add Tauri. Phase 8 expands safety UX/policy but does not permit unsafe actions in earlier MCP phases. Playwright sidecars, vision, and recorder packages remain optional until needed.

---

# 58. MVP Definition

Do not call it MVP until this works:

```text
User opens arbitrary normal Windows app.

AI calls desktop_snapshot.

AI sees meaningful interactive controls.

AI locates a target by semantic properties.

AI clicks/fills/selects it without coordinate knowledge.

AI waits for the resulting UI change.

AI can move across multiple windows/apps.

AI can use screenshot/vision only when semantic control fails.

User can cancel the agent instantly.
```

---

# 59. Non-Goals for MVP

Do NOT start with:

```text
voice conversation
animated mascot
cloud accounts
subscriptions
team collaboration
mobile companion
remote computer control
marketplace
huge plugin ecosystem
autonomous background scheduling
full OCR pipeline
perfect game automation
perfect Adobe canvas automation
```

These can distract from the real moat:

```text
reliable semantic Windows control
```

---

# 60. First Demonstration

The first public-quality demo should be simple:

Prompt:

```text
Open Notepad, type a short paragraph, save it to my Desktop as demo.txt, then open File Explorer and show me the file.
```

Expected tool flow:

```text
app_launch notepad
desktop_snapshot
desktop_fill editor
desktop_hotkey Ctrl+Shift+S
desktop_wait_for Save As
desktop_snapshot
desktop_fill File name
desktop_click Save
desktop_wait_for Save As missing
app_launch explorer Desktop
desktop_wait_for Explorer
desktop_find text demo.txt
overlay_highlight demo.txt
```

This proves cross-window automation without requiring vision.

---

# 61. Second Demonstration

Prompt:

```text
Open Settings and show me where I change my display scaling, but do not change it.
```

Expected:

```text
launch Settings
snapshot
semantic navigation
locate scaling control
overlay arrow/highlight
no modifying action
```

This proves assistant/tutorial mode.

---

# 62. Third Demonstration

Prompt:

```text
Go to a website, download an image, open it in Paint, resize it, and save a copy.
```

Expected:

```text
browser DOM automation
download
filesystem awareness
launch Paint
UIA menus/dialogs
physical/vision fallback only if canvas requires it
save
```

This demonstrates mixed browser + desktop control.

---

# 63. Claude Code Implementation Instructions

When implementing this project:

1. Do not build all phases in one giant change.
2. Work phase by phase.
3. Keep the Cargo workspace building after every phase.
4. Add tests before expanding behavior.
5. Never expose raw COM objects outside the owning UIA worker.
6. Keep tool contracts stable.
7. Avoid coordinate automation unless semantic methods fail.
8. Never swallow automation exceptions.
9. Convert internal failures into typed Winwright errors.
10. Add cancellation support to all waits/actions.
11. Redact sensitive values in logs/tool responses.
12. Do not silently elevate privileges.
13. Do not execute arbitrary shell commands merely because a model asked.
14. Add an integration test for every bug fixed.
15. Prefer event-driven waits over fixed Tokio sleeps.
16. Keep MCP tools thin; business logic belongs in Core.
17. Do not put model reasoning inside the automation engine.
18. Do not build voice/mascot/UI polish until MCP semantic automation works.
19. Document any app-specific workaround.
20. Maintain a compatibility matrix.

Rust-specific rules:

21. Keep COM on the dedicated MTA worker and unsafe interop in audited platform modules.
22. Use bounded channels, monotonic deadlines, and an independent emergency cleanup path.
23. Do not retry timed-out actions unless nonexecution is known.
24. Keep the core runnable without Tauri, WebView2, Node, or external model services.
25. Commit tested Cargo/toolchain pins and run formatting, Clippy, and relevant tests.
26. Treat UIA text, screenshots, browser pages, and app content as untrusted data that cannot grant permissions.
27. WPF/WinForms/WinUI are compatibility targets with separate fixture requirements.

---

# 64. Suggested Core Traits and Types

The snippets describe Rust contracts. Referenced DTOs live in `winwright-contracts`; only the UIA worker owns COM objects. Use an object-safe boxed future at pluggable async boundaries; concrete internal services can use ordinary async methods.

```rust
use std::{future::Future, pin::Pin, time::{Duration, Instant}};
use tokio_util::sync::CancellationToken;

pub type WinwrightResult<T> = Result<T, WinwrightError>;
pub type BackendFuture<'a, T> =
    Pin<Box<dyn Future<Output = WinwrightResult<T>> + Send + 'a>>;

#[derive(Clone)]
pub struct OperationContext {
    pub session_id: SessionId,
    pub action_epoch: u64,
    pub deadline: Instant,
    pub cancel: CancellationToken,
}

// Internal handle, never a raw COM interface or pointer.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct ElementKey {
    pub worker_epoch: u64,
    pub slot: u64,
}

pub trait DesktopAutomation: Send + Sync {
    fn snapshot<'a>(&'a self, request: SnapshotRequest,
                    ctx: &'a OperationContext) -> BackendFuture<'a, DesktopSnapshot>;
    fn find<'a>(&'a self, locator: ElementLocator,
                ctx: &'a OperationContext) -> BackendFuture<'a, ElementQueryResult>;
    fn execute<'a>(&'a self, action: DesktopAction,
                   ctx: &'a OperationContext) -> BackendFuture<'a, ActionResult>;
    fn wait_for<'a>(&'a self, request: WaitRequest,
                    ctx: &'a OperationContext) -> BackendFuture<'a, WaitResult>;
}

pub trait UiAutomationBackend: Send + Sync {
    fn capture_tree<'a>(&'a self, request: UiTreeRequest,
                        ctx: &'a OperationContext) -> BackendFuture<'a, UiTree>;
    fn resolve<'a>(&'a self, identity: ElementIdentity,
                   ctx: &'a OperationContext) -> BackendFuture<'a, Option<ElementKey>>;
    fn patterns<'a>(&'a self, key: ElementKey,
                    ctx: &'a OperationContext) -> BackendFuture<'a, Vec<UiPattern>>;
    fn execute_pattern<'a>(&'a self, key: ElementKey, action: UiPatternAction,
                           ctx: &'a OperationContext) -> BackendFuture<'a, UiActionResult>;
    fn subscribe<'a>(&'a self, request: EventSubscriptionRequest,
                     ctx: &'a OperationContext) -> BackendFuture<'a, SubscriptionId>;
    fn unsubscribe<'a>(&'a self, id: SubscriptionId,
                       ctx: &'a OperationContext) -> BackendFuture<'a, ()>;
}
```

`UiAutomationBackend` is a thread-safe mailbox proxy. The worker receives owned commands via a bounded channel, replies with `tokio::sync::oneshot`, and keeps elements/subscriptions in epoch-qualified slots. Never send live COM objects across the boundary.

```rust
pub trait InputBackend: Send + Sync {
    fn click<'a>(&'a self, point: PhysicalScreenPoint, button: MouseButton,
                 count: u32, ctx: &'a OperationContext) -> BackendFuture<'a, ()>;
    fn type_text<'a>(&'a self, text: &'a str,
                     ctx: &'a OperationContext) -> BackendFuture<'a, ()>;
    fn hotkey<'a>(&'a self, keys: &'a [KeyCode],
                  ctx: &'a OperationContext) -> BackendFuture<'a, ()>;
    fn drag<'a>(&'a self, from: PhysicalScreenPoint, to: PhysicalScreenPoint,
                duration: Duration, ctx: &'a OperationContext) -> BackendFuture<'a, ()>;
}

pub trait CaptureService: Send + Sync {
    fn capture_desktop<'a>(&'a self, request: DesktopCaptureRequest,
                           ctx: &'a OperationContext) -> BackendFuture<'a, CapturedImage>;
    fn capture_window<'a>(&'a self, window: WindowIdentity,
                          ctx: &'a OperationContext) -> BackendFuture<'a, CapturedImage>;
    fn capture_element<'a>(&'a self, element: ElementIdentity,
                           ctx: &'a OperationContext) -> BackendFuture<'a, CapturedImage>;
}

pub trait PermissionService: Send + Sync {
    fn evaluate<'a>(&'a self, action: &'a ProposedAction,
                    session: &'a AutomationSession,
                    ctx: &'a OperationContext) -> BackendFuture<'a, PermissionDecision>;
}

pub trait VisionGrounder: Send + Sync {
    fn find<'a>(&'a self, image: &'a CapturedImage, instruction: &'a str,
                ctx: &'a OperationContext) -> BackendFuture<'a, Vec<VisualTarget>>;
}
```

`WindowIdentity` includes validated process/window identity, with HWND reuse checks inside the platform layer. `CapturedImage` holds owned bytes or a protected artifact handle plus dimensions, physical origin, coordinate space, and timestamp. Input tracks synthesized keys/buttons and exposes an emergency release path independent of UIA. Map `thiserror` variants to stable wire codes from Section 32.

---

# 65. Locator Data Model

Use owned Rust/serde types. `Box` breaks recursive type sizing and camelCase preserves the JSON wire shape:

```rust
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MatchMode {
    #[default] Exact,
    Contains,
    Regex,
}

fn default_visible_only() -> bool { true }

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementLocator {
    pub role: Option<String>,
    pub name: Option<String>,
    pub text: Option<String>,
    pub automation_id: Option<String>,
    pub class_name: Option<String>,
    pub framework_id: Option<String>,
    pub label: Option<String>,
    pub ancestor: Option<Box<ElementLocator>>,
    #[serde(default, rename = "match")]
    pub match_mode: MatchMode,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_visible_only")]
    pub visible_only: bool,
    pub nth: Option<usize>,
}
```

Bound ancestor depth, strings/regex complexity, and result count. `nth` is explicit and zero-based. The flat `desktop_find` schema from Section 13 normalizes into this model (including `exact`); caller window/session scope is separate. All supplied predicates must match. Rank exact AutomationId, role, exact name, framework, ancestor, then geometry only as a last tie-breaker. Never silently choose equally strong matches.

---

# 66. Security Data Model

```rust
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ActionRisk { ReadOnly, Normal, Sensitive, Destructive, Privileged }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecision { Allow, Confirm, Deny }
```

Every MCP/API/CLI/workflow call produces a `ProposedAction` before execution. Risk follows intended effect and resolved target: a click on Send/Delete/Buy may need confirmation. Confirmation binds one session, target fingerprint, operation/arguments, expiry, and a one-use nonce; revalidate immediately before execution. Only trusted local UI or an explicit user-controlled CLI flow can approve. Model-supplied flags cannot grant approval. Secret values use non-serializable types/opaque references and never derive Debug. Preserve the original credential and elevation defaults.

---

# 67. Tool Result Philosophy

Tool results should be concise but diagnostic.

Good:

```json
{
  "success": true,
  "target": "Button \"Save\"",
  "method": "InvokePattern",
  "verified": true,
  "diff": "+ Window \"Confirm Save\""
}
```

Bad:

```json
{
  "success": true,
  "uiaElementProperties": "...500 lines..."
}
```

Only return detailed diagnostics when requested.

---

# 68. Browser vs Desktop Rule

If browser content can be controlled with Playwright:

```text
use Playwright
```

If interaction is outside the webpage DOM:

```text
use Winwright
```

Examples for Winwright:

```text
browser toolbar
downloads panel if DOM attachment unavailable
native file picker
Windows permission dialog
Open With
print dialog
external app launched from browser
```

---

# 69. Local-First Privacy Defaults

Default:

```text
no continuous screen recording
no screenshot persistence
no cloud sync
no telemetry
no remote server requirement
local logs only
```

If an external vision/model API is enabled, the user must know when screen data is being sent to it.

Architect the project so fully local use remains possible.

---

# 70. Long-Term Expansion

Possible later features:

```text
voice mode
screen explanation mode
background deterministic workflows
application-specific adapters
local vision models
semantic OCR
task templates
skill marketplace
remote control with explicit pairing
multi-machine agent
Windows shell context-menu integration
Visual Studio / IDE integrations
automatic locator healing
workflow debugger
workflow time-travel/replay
```

Do not implement these before semantic desktop automation is stable.

---

# 71. Product Positioning

The most accurate conceptual description is:

> Playwright for Windows desktop applications, exposed to AI through MCP.

The differentiator should not be “the AI can click things.”

The differentiator should be:

> The AI receives a compact semantic representation of Windows and can interact with it deterministically.

---

# 72. Critical Engineering Rule

When implementing an action, always ask:

```text
Can Windows tell us what this control IS and what operation it SUPPORTS?
```

If yes:

```text
use that semantic operation
```

Only ask:

```text
Where on the screen should we click?
```

when semantic automation has failed.

That single rule should govern the project.
