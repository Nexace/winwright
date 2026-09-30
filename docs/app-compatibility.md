# App Compatibility Matrix

Observed on Windows 11 (build 26300), 125% scale, release build, 2026-09-30.
"Tree" = semantic snapshot quality. Action columns fill in from Phase 3.

| App | Framework | Tree | Locators | Click | Fill | Scroll | Text | Dialogs | Menus | Fallback needed | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Notepad (Win11, tabbed) | XAML | good | — | — | — | — | — | — | — | — | Document, tabs, menu bar, status text all named. |
| Settings | XAML (ApplicationFrameHost) | good | — | — | — | — | — | — | — | — | Top-level window is owned by `ApplicationFrameHost.exe`, not `SystemSettings.exe`; match by title. Nested `WINDOW` hosts for the title bar and content. |
| File Explorer | XAML + DirectUI | good | — | — | — | — | — | — | — | — | Grid cells fold into rows; writable `Name` cell stays a separate EDIT. Dates carry bidi marks (stripped in snapshots). |
| OpenCode / Electron | Chrome | readable | — | — | — | — | — | — | — | — | Deep unnamed `Pane` chains above `RootWebArea`; flattened in snapshots. |

## Known platform quirks

- UWP/XAML apps hosted by `ApplicationFrameHost.exe` report the host as the window's process.
  Resolve the real app process from the `Windows.UI.Core.CoreWindow` child (planned).
- Win11 Notepad restores previous-session tabs on launch; tests must not assume an empty document.
