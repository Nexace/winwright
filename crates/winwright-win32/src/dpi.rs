use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Declares Per-Monitor-V2 awareness so UIA rectangles, cursor positions, and window bounds
/// are all physical pixels (spec §43). Must run before any HWND is created.
/// Returns false when awareness was already fixed (by a manifest or an earlier call).
pub fn enable_per_monitor_dpi_awareness() -> bool {
    // SAFETY: plain process-wide flag; no pointers are passed.
    match unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        Ok(()) => true,
        Err(err) => {
            tracing::debug!(%err, "DPI awareness already set");
            false
        }
    }
}
