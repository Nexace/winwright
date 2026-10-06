//! `winwright doctor`: checks this PC once, before an AI relies on it. Every monitor's size and
//! scaling, then on each an overlay drawn and captured back; UI Automation and the click
//! watcher. Shows a small box on each monitor for a moment and takes no mouse or keyboard input.

use std::sync::Arc;
use std::time::Duration;

use winwright_contracts::WinwrightError;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::capture::{
    CaptureRequest, CaptureService, CaptureTarget, ImageFormat, MonitorInfo,
};
use winwright_contracts::config::Config;
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::ids::SessionId;
use winwright_contracts::overlay::{OverlayRequest, OverlayService, OverlayStyle};
use winwright_contracts::snapshot::SnapshotRequest;
use winwright_core::Engine;

const BOX: (i32, i32) = (200, 100);
/// Long enough for an overlay to be composed onto the screen.
const SETTLE: Duration = Duration::from_millis(300);
const ATTEMPTS: u32 = 3;

/// The highlight's border at `dpi`, as the overlay draws it (3 px at 100 %).
fn border(dpi: u32) -> i32 {
    ((3 * dpi as i32 + 48) / 96).max(1)
}

/// Where a highlight of `target` must land on `monitor`: the target plus its border.
fn expected(target: PhysicalRect, monitor: &MonitorInfo) -> PhysicalRect {
    let b = border(monitor.dpi);
    let r = PhysicalRect::new(
        target.left - b,
        target.top - b,
        target.right + b,
        target.bottom + b,
    );
    PhysicalRect::new(
        r.left.max(monitor.bounds.left),
        r.top.max(monitor.bounds.top),
        r.right.min(monitor.bounds.right),
        r.bottom.min(monitor.bounds.bottom),
    )
}

fn centered_box(work: PhysicalRect) -> PhysicalRect {
    let c = work.center();
    PhysicalRect::new(
        c.x - BOX.0 / 2,
        c.y - BOX.1 / 2,
        c.x + BOX.0 / 2,
        c.y + BOX.1 / 2,
    )
}

struct Report {
    failed: u32,
}

impl Report {
    fn check(&mut self, what: &str, result: Result<String, String>) {
        match result {
            Ok(detail) => println!("  ok      {what}: {detail}"),
            Err(problem) => {
                self.failed += 1;
                println!("  FAILED  {what}: {problem}");
            }
        }
    }
}

async fn shot(
    capture: &dyn CaptureService,
    rect: PhysicalRect,
    ctx: &OperationContext,
) -> Result<Vec<u8>, String> {
    let request = CaptureRequest {
        target: CaptureTarget::Region(rect),
        format: ImageFormat::Png,
        quality: 100,
    };
    capture
        .capture(request, ctx)
        .await
        .map(|image| image.bytes)
        .map_err(|e| e.to_string())
}

async fn check_monitor(
    m: &MonitorInfo,
    overlay: &winwright_overlay::NativeOverlay,
    capture: &dyn CaptureService,
    ctx: &OperationContext,
    report: &mut Report,
) {
    let b = m.bounds;
    println!(
        "Monitor {} {}{}: {}x{} at {},{}, {} % scaling ({} dpi), work area {}x{}",
        m.index,
        m.name,
        if m.primary { " (main)" } else { "" },
        b.width(),
        b.height(),
        b.left,
        b.top,
        m.scale_percent,
        m.dpi,
        m.work_area.width(),
        m.work_area.height(),
    );
    let target = centered_box(m.work_area);
    let want = expected(target, m);
    let mut placed = false;
    // Something else may be moving under the box (a video, streaming text): try a few times.
    for attempt in 1..=ATTEMPTS {
        let before = shot(capture, want, ctx).await;
        let id = match overlay.show(OverlayRequest {
            rect: target,
            style: OverlayStyle::Highlight,
            label: None,
            step: None,
            steps: None,
            color: 0x00FF_00FF,
            duration_ms: Some(5_000),
        }) {
            Ok(id) => id,
            Err(e) => return report.check("overlay", Err(e.to_string())),
        };
        if !placed {
            placed = true;
            let at = match overlay.window_rect(id) {
                Ok(Some(got)) if got == want => Ok(format!(
                    "drawn exactly in place with a {} px border",
                    border(m.dpi)
                )),
                Ok(Some(got)) => Err(format!("drawn at {got:?}, expected {want:?}")),
                Ok(None) => Err("nothing was drawn".into()),
                Err(e) => Err(e.to_string()),
            };
            report.check("overlay", at);
        }
        tokio::time::sleep(SETTLE).await;
        let with_box = shot(capture, want, ctx).await;
        let _ = overlay.clear(Some(id));
        tokio::time::sleep(SETTLE).await;
        let after = shot(capture, want, ctx).await;
        let seen = match (before, with_box, after) {
            (Ok(before), Ok(with_box), Ok(after)) => {
                if before != after {
                    if attempt < ATTEMPTS {
                        continue;
                    }
                    println!(
                        "  skipped screenshot: the screen kept changing at the middle of this \
                         monitor; move busy windows aside and run it again"
                    );
                    return;
                }
                if with_box == before {
                    Err("a screenshot of that spot does not show the box".into())
                } else {
                    Ok("a screenshot of that spot shows the box".into())
                }
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
        };
        return report.check("screenshot", seen);
    }
}

pub async fn run(config: Config) -> Result<(), WinwrightError> {
    let mut report = Report { failed: 0 };
    let engine = Engine::new(
        config,
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(winwright_uia::UiaBackend::start()?),
    );
    let session = engine.session(&SessionId::parse("doctor").expect("valid id"), "doctor")?;
    let ctx = session.operation(Duration::from_secs(60))?;
    let capture = winwright_capture::WgcCapture::start()?;
    let monitors = capture.monitors()?;
    let scales: std::collections::BTreeSet<u32> =
        monitors.iter().map(|m| m.scale_percent).collect();
    println!(
        "{} monitor(s){}",
        monitors.len(),
        if scales.len() > 1 {
            " with different scaling: each is checked on its own"
        } else {
            ""
        }
    );
    let ui = winwright_overlay::NativeUi::start()?;
    for m in &monitors {
        check_monitor(m, &ui.overlay, &capture, &ctx, &mut report).await;
    }

    println!("This PC");
    let watch = ui
        .overlay
        .watch_pointer(Box::new(|_| {}))
        .map(|_| "can see where you click during a lesson (never your keys)".to_owned())
        .map_err(|e| e.to_string());
    report.check("click watcher", watch);
    let read = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .map(|s| {
            format!(
                "read {} lines of the window in front",
                s.tree.lines().count()
            )
        })
        .map_err(|e| e.to_string());
    report.check("UI Automation", read);

    if report.failed == 0 {
        println!("All checks passed.");
        Ok(())
    } else {
        Err(WinwrightError::invalid(format!(
            "{} check(s) failed; see above",
            report.failed
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(bounds: PhysicalRect, dpi: u32) -> MonitorInfo {
        MonitorInfo {
            index: 1,
            name: "\\\\.\\DISPLAY2".into(),
            bounds,
            work_area: bounds,
            dpi,
            scale_percent: dpi * 100 / 96,
            primary: false,
        }
    }

    #[test]
    fn the_box_and_its_border_follow_each_monitors_scaling() {
        assert_eq!(
            (border(96), border(120), border(144), border(192)),
            (3, 4, 5, 6)
        );
        // A 150 % monitor left of the main one (negative coordinates).
        let left = monitor(PhysicalRect::new(-2560, 0, 0, 1440), 144);
        let target = centered_box(left.work_area);
        assert_eq!(target, PhysicalRect::new(-1380, 670, -1180, 770));
        assert_eq!(
            expected(target, &left),
            PhysicalRect::new(-1385, 665, -1175, 775)
        );
        // Clipped to its monitor.
        let edge = PhysicalRect::new(-2560, 0, -2400, 50);
        assert_eq!(
            expected(edge, &left),
            PhysicalRect::new(-2560, 0, -2395, 55)
        );
    }
}
