//! Winwright MCP server (spec §12, §13, §67). Tools stay thin: parse model-friendly input,
//! call the engine, render compact results. Business logic lives in `winwright-core`.
//! stdout carries MCP frames only; logs go to stderr.

mod inputs;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Serialize;
use winwright_contracts::WinwrightError;
use winwright_contracts::action::DesktopAction;
use winwright_contracts::capture::ImageFormat;
use winwright_contracts::ids::SessionId;
use winwright_contracts::input::parse_chord;
use winwright_contracts::locator::FindResult;
use winwright_contracts::snapshot::DesktopSnapshot;
use winwright_core::session::Session;
use winwright_core::{Engine, InspectRequest};

pub use inputs::*;

type ToolResult = Result<CallToolResult, ErrorData>;

const INSTRUCTIONS: &str = "Winwright operates Windows apps through UI Automation.\n\
1. desktop_snapshot shows the active window as a compact tree; interactive elements carry refs like [e12].\n\
2. Act by ref (desktop_click, desktop_fill, desktop_select, desktop_check, ...). Locators (role/name/label/window) also work when you have no ref.\n\
3. Use desktop_wait_for instead of sleeping, then desktop_snapshot with diff=true to see only what changed.\n\
4. Prefer app_launch and filesystem_operation over clicking through the shell.\n\
5. Use desktop_screenshot only when the tree lacks what you need.\n\
Errors are JSON with a code and a hint. CONFIRMATION_REQUIRED means the user must approve: do not work around it. \
CANCELLED after an emergency stop means the user stopped you: stop and ask them before doing anything else.";

fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s)])
}

fn json<T: Serialize>(value: &T) -> CallToolResult {
    text(serde_json::to_string(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")))
}

/// Tool-level error: the model sees the stable code, message, hint and candidate refs.
fn fail(err: WinwrightError) -> CallToolResult {
    let payload = serde_json::to_string(&err.payload()).unwrap_or_else(|_| err.to_string());
    CallToolResult::error(vec![ContentBlock::text(payload)])
}

fn render_snapshot(s: &DesktopSnapshot) -> String {
    let mut out = format!("snapshot {}", s.generation);
    if let Some(w) = &s.active_window {
        out.push_str(&format!(
            " | window {:?} ({}) [{}]",
            w.title, w.process, w.reference
        ));
    }
    out.push('\n');
    out.push_str(s.diff.as_deref().unwrap_or(&s.tree));
    if s.truncated {
        out.push_str("(truncated)\n");
    }
    for w in &s.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    out
}

fn render_found(f: &FindResult) -> String {
    let mut out = format!("{} match(es)\n", f.count);
    for m in &f.matches {
        out.push_str(&format!("{} {}", m.reference, m.path));
        if !m.automation_id.is_empty() {
            out.push_str(&format!(" id={:?}", m.automation_id));
        }
        if let Some(v) = &m.value {
            out.push_str(&format!(" value={v:?}"));
        }
        if !m.enabled {
            out.push_str(" disabled");
        }
        if !m.visible {
            out.push_str(" offscreen");
        }
        out.push('\n');
    }
    for w in &f.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    out
}

#[derive(Clone)]
pub struct WinwrightMcp {
    engine: Arc<Engine>,
    session: Arc<Mutex<Arc<Session>>>,
    tool_router: ToolRouter<Self>,
}

fn session_id() -> SessionId {
    SessionId::parse("mcp").expect("valid id")
}

impl WinwrightMcp {
    pub fn new(engine: Arc<Engine>) -> Result<Self, WinwrightError> {
        let session = engine.session(&session_id(), "mcp")?;
        Ok(Self {
            engine,
            session: Arc::new(Mutex::new(session)),
            tool_router: Self::tool_router(),
        })
    }

    /// The current session. After an emergency stop it stays cancelled (every tool fails with
    /// CANCELLED) until the user re-enables Winwright, which yields a fresh session here.
    fn sess(&self) -> Arc<Session> {
        let mut current = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if current.is_cancelled()
            && let Ok(fresh) = self.engine.session(&session_id(), "mcp")
        {
            *current = fresh;
        }
        Arc::clone(&current)
    }

    async fn act(&self, action: Result<DesktopAction, WinwrightError>) -> CallToolResult {
        match action {
            Ok(action) => match self.engine.execute(&self.sess(), action).await {
                Ok(result) => json(&result),
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        }
    }
}

#[tool_router]
impl WinwrightMcp {
    #[tool(
        description = "Compact semantic snapshot of the active window (or `window`, or a `ref` subtree). \
        Interactive elements get refs like [e12] for the other tools. diff=true returns only changes since your previous snapshot."
    )]
    async fn desktop_snapshot(&self, Parameters(input): Parameters<SnapshotInput>) -> ToolResult {
        Ok(
            match self.engine.snapshot(&self.sess(), input.request()).await {
                Ok(s) => text(render_snapshot(&s)),
                Err(e) => fail(e),
            },
        )
    }

    #[tool(
        description = "Find elements by role/name/text/automationId/label (exact by default; exact=false for substring). Returns refs."
    )]
    async fn desktop_find(&self, Parameters(input): Parameters<FindInput>) -> ToolResult {
        Ok(match input.request() {
            Ok(req) => match self.engine.find(&self.sess(), req).await {
                Ok(found) => text(render_found(&found)),
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        })
    }

    #[tool(description = "List visible top-level windows (the foreground window is marked).")]
    async fn desktop_windows(&self) -> ToolResult {
        Ok(match self.engine.list_windows() {
            Ok(ws) => text(
                ws.iter()
                    .map(|w| {
                        format!(
                            "{}{:?} ({}) hwnd={:#x}{}{}",
                            if w.foreground { "* " } else { "  " },
                            w.title,
                            w.process_name,
                            w.hwnd,
                            if w.minimized { " minimized" } else { "" },
                            if w.maximized { " maximized" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "Detailed properties of one element: by ref, at screen point x,y, or the focused element."
    )]
    async fn desktop_inspect(&self, Parameters(input): Parameters<InspectInput>) -> ToolResult {
        let request = match (input.reference, input.x.zip(input.y)) {
            (Some(r), _) => InspectRequest::Ref(r),
            (None, Some((x, y))) => {
                InspectRequest::Point(winwright_contracts::geometry::PhysicalPoint { x, y })
            }
            (None, None) => InspectRequest::Focused,
        };
        Ok(match self.engine.inspect(&self.sess(), request).await {
            Ok(d) => json(&d),
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "Click an element. Uses InvokePattern (or selection/toggle/expand) first; real mouse input only as a fallback, \
        or when button/doubleClick/forcePhysical ask for it."
    )]
    async fn desktop_click(&self, Parameters(input): Parameters<ClickInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Set the text of an input (ValuePattern, else keyboard). append=true keeps existing text."
    )]
    async fn desktop_fill(&self, Parameters(input): Parameters<FillInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Type text with the real keyboard into the target (focused first) or the focused control."
    )]
    async fn desktop_type(&self, Parameters(input): Parameters<TypeInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Press a key chord such as \"Ctrl+S\", \"Enter\", \"Alt+F4\", optionally after focusing a target."
    )]
    async fn desktop_press(&self, Parameters(input): Parameters<PressInput>) -> ToolResult {
        let action = parse_chord(&input.keys)
            .map_err(WinwrightError::invalid)
            .and_then(|keys| {
                Ok(DesktopAction::Press {
                    target: input.target.optional()?,
                    keys,
                })
            });
        Ok(self.act(action).await)
    }

    #[tool(
        description = "Select `option` inside a combo box/list/tab/tree target, or select the target item itself."
    )]
    async fn desktop_select(&self, Parameters(input): Parameters<SelectInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Check (default), uncheck, or toggle a checkbox; `check` on a radio button selects it."
    )]
    async fn desktop_check(&self, Parameters(input): Parameters<CheckInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(description = "Expand (default) or collapse a tree item, combo box, or menu.")]
    async fn desktop_expand(&self, Parameters(input): Parameters<ExpandInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Scroll a container by pages in a direction, or scroll the target itself into view (intoView=true)."
    )]
    async fn desktop_scroll(&self, Parameters(input): Parameters<ScrollInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(description = "Move keyboard focus to an element.")]
    async fn desktop_focus(&self, Parameters(input): Parameters<FocusInput>) -> ToolResult {
        Ok(self
            .act(
                input
                    .target
                    .required()
                    .map(|target| DesktopAction::Focus { target }),
            )
            .await)
    }

    #[tool(
        description = "Read an element's full text (TextPattern, then value, then name). Password fields are refused."
    )]
    async fn desktop_read_text(&self, Parameters(input): Parameters<ReadTextInput>) -> ToolResult {
        Ok(self.act(input.action()).await)
    }

    #[tool(
        description = "Wait (no fixed sleeps) until an element reaches a state (exists, missing, visible, hidden, enabled, disabled, focused, value, text) \
        or a window opens/closes (window-open, window-closed with windowTitle)."
    )]
    async fn desktop_wait_for(&self, Parameters(input): Parameters<WaitInput>) -> ToolResult {
        Ok(match input.request() {
            Ok(req) => match self.engine.wait_for(&self.sess(), req).await {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "Focus, move, resize, minimize, maximize, restore, or close a top-level window."
    )]
    async fn window_control(&self, Parameters(input): Parameters<WindowInput>) -> ToolResult {
        Ok(match input.action() {
            Ok(action) => match self.engine.window_action(&self.sess(), action).await {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "On-demand screenshot of the active window, a window, an element (ref), a monitor, a region, or the desktop. \
        Costly in tokens: use only when the snapshot tree is not enough. Defaults to JPEG."
    )]
    async fn desktop_screenshot(
        &self,
        Parameters(input): Parameters<ScreenshotInput>,
    ) -> ToolResult {
        let request = match input.request() {
            Ok(r) => r,
            Err(e) => return Ok(fail(e)),
        };
        let mime = match request.format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        };
        Ok(match self.engine.screenshot(&self.sess(), request).await {
            Ok(img) => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                    mime,
                ),
                ContentBlock::text(format!(
                    "{}x{} at {},{} ({} dpi); pixel (0,0) = desktop ({},{})",
                    img.width,
                    img.height,
                    img.origin.x,
                    img.origin.y,
                    img.dpi,
                    img.origin.x,
                    img.origin.y
                )),
            ]),
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "Point at an element for the user with a click-through overlay (highlight, arrow, or click marker) and optional caption. Nothing is clicked."
    )]
    async fn overlay_highlight(&self, Parameters(input): Parameters<HighlightInput>) -> ToolResult {
        Ok(match input.request() {
            Ok(req) => match self.engine.highlight(&self.sess(), req).await {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        })
    }

    #[tool(description = "Remove all overlays.")]
    async fn overlay_clear(&self) -> ToolResult {
        Ok(match self.engine.clear_overlays(None) {
            Ok(()) => text("cleared"),
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "Launch an app (notepad.exe), shell URI (ms-settings:display) or folder path. Prefer this over clicking through the Start menu."
    )]
    async fn app_launch(&self, Parameters(input): Parameters<LaunchInput>) -> ToolResult {
        Ok(
            match self.engine.launch_app(&self.sess(), input.request()).await {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
        )
    }

    #[tool(description = "List running processes (pid, name, integrity level).")]
    async fn process_list(&self) -> ToolResult {
        Ok(match self.engine.process_list() {
            Ok(list) => text(
                list.iter()
                    .map(|p| {
                        format!(
                            "{} {} {}",
                            p.process_id,
                            p.name,
                            p.integrity.as_deref().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => fail(e),
        })
    }

    #[tool(
        description = "File operations without touching Explorer: list, metadata, copy, move, rename, delete (Recycle Bin, needs user confirmation), \
        createDirectory, search, knownFolder (Desktop, Documents, Downloads, ...)."
    )]
    async fn filesystem_operation(&self, Parameters(input): Parameters<FileInput>) -> ToolResult {
        Ok(
            match self
                .engine
                .file_operation(&self.sess(), input.operation)
                .await
            {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
        )
    }

    #[tool(
        description = "Run a program with an argument list (no shell). Disabled unless the user enabled it in config; every run needs user confirmation."
    )]
    async fn shell_execute(&self, Parameters(input): Parameters<ExecInput>) -> ToolResult {
        Ok(
            match self.engine.exec(&self.sess(), input.request()).await {
                Ok(r) => json(&r),
                Err(e) => fail(e),
            },
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WinwrightMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("winwright", env!("CARGO_PKG_VERSION"))
                    .with_title("Winwright")
                    .with_description("Semantic Windows desktop automation over UI Automation"),
            )
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves MCP over stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    engine: Arc<Engine>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = WinwrightMcp::new(engine)?;
    let running = server.serve(rmcp::transport::stdio()).await?;
    let reason = running.waiting().await?;
    tracing::info!(?reason, "MCP client disconnected");
    Ok(())
}
