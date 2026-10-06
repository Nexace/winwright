//! The dedicated MTA thread that owns every COM, WinRT, D3D11, and WIC object (spec §5).
//!
//! Commands arrive over a bounded channel with owned data and a oneshot reply; replies carry
//! owned encoded bytes only.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use winwright_contracts::capture::{CaptureRequest, CaptureTarget, CapturedImage};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::ComApartment;
use crate::monitors::{self, Monitor, PhysicalDpiScope};
use crate::raster::{
    Bgra, LocalRect, blit, force_opaque, plan_region, region_size, top_left, union,
};
use crate::wgc::{Items, Source, Wgc};
use crate::wic::Wic;
use crate::window;

pub struct Deadline {
    /// When the operation started, so a timeout reports the real time spent.
    pub started: Instant,
    pub at: Instant,
    pub cancel: CancellationToken,
}

impl Deadline {
    /// Called before dispatch and between monitor captures.
    fn check(&self, operation: &str) -> WinwrightResult<()> {
        if self.cancel.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        if Instant::now() >= self.at {
            return Err(WinwrightError::Timeout {
                operation: operation.to_owned(),
                elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }
}

pub enum Command {
    Capture {
        request: CaptureRequest,
        deadline: Deadline,
        reply: oneshot::Sender<WinwrightResult<CapturedImage>>,
    },
}

struct Worker {
    wgc: Wgc,
    wic: Wic,
    items: Items,
}

pub fn run(mut rx: mpsc::Receiver<Command>, ready: std::sync::mpsc::Sender<WinwrightResult<()>>) {
    // Drop order matters: the apartment is declared first so it is dropped last.
    let _apartment = match ComApartment::init_mta() {
        Ok(a) => a,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // Physical pixels for every monitor/window rectangle this thread reads.
    let _physical = PhysicalDpiScope::enter();
    let mut worker = match Wgc::new().and_then(|wgc| {
        Ok(Worker {
            wgc,
            wic: Wic::new()?,
            items: Items::default(),
        })
    }) {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    tracing::debug!("capture worker ready");

    while let Some(command) = rx.blocking_recv() {
        match command {
            Command::Capture {
                request,
                deadline,
                reply,
            } => {
                if reply.is_closed() {
                    continue; // The caller gave up while the command was queued.
                }
                let started = Instant::now();
                let result = worker.capture(&request, &deadline);
                match &result {
                    Ok(image) => tracing::debug!(
                        width = image.width,
                        height = image.height,
                        bytes = image.bytes.len(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "captured"
                    ),
                    Err(e) => tracing::debug!(%e, "capture failed"),
                }
                let _ = reply.send(result);
            }
        }
    }
    tracing::debug!("capture worker stopping");
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Worker {
    fn capture(
        &mut self,
        request: &CaptureRequest,
        deadline: &Deadline,
    ) -> WinwrightResult<CapturedImage> {
        deadline.check("capture")?;
        let (mut image, origin) = match request.target {
            CaptureTarget::Window(hwnd) => self.window(hwnd, deadline)?,
            CaptureTarget::Monitor(index) => self.monitor(index, deadline)?,
            CaptureTarget::Region(rect) => {
                let monitors = monitors::enumerate()?;
                (self.region(rect, &monitors, deadline)?, top_left(&rect))
            }
            CaptureTarget::Desktop => {
                let monitors = monitors::enumerate()?;
                let bounds: Vec<PhysicalRect> = monitors.iter().map(|m| m.info.bounds).collect();
                let desktop = union(&bounds).ok_or_else(|| WinwrightError::CaptureFailed {
                    reason: "no monitors are attached".into(),
                })?;
                (
                    self.region(desktop, &monitors, deadline)?,
                    top_left(&desktop),
                )
            }
        };
        let timestamp_ms = now_ms();
        deadline.check("capture")?;
        force_opaque(&mut image.pixels);
        let (width, height) = request.fit.map_or((image.width, image.height), |fit| {
            fit.size(image.width, image.height)
        });
        let bytes = self
            .wic
            .encode(&image, (width, height), request.format, request.quality)?;
        Ok(CapturedImage {
            bytes,
            format: request.format,
            width,
            height,
            physical_width: image.width,
            physical_height: image.height,
            origin,
            dpi: monitors::dpi_at(origin),
            timestamp_ms,
        })
    }

    /// The captured size is authoritative: when it differs from the DWM frame bounds (a
    /// resize racing the capture), the image keeps its own size and only the origin comes
    /// from the frame bounds.
    fn window(&mut self, hwnd: u64, deadline: &Deadline) -> WinwrightResult<(Bgra, PhysicalPoint)> {
        let handle = window::capturable(hwnd)?;
        let bounds = window::frame_bounds(handle);
        let image = self.grab(Source::Window(handle.0 as usize), bounds, None, deadline)?;
        Ok((image, top_left(&bounds)))
    }

    /// Grabs `source` with a kept capture item when one fits, else a new one (kept after).
    fn grab(
        &mut self,
        source: Source,
        bounds: PhysicalRect,
        crop: Option<LocalRect>,
        deadline: &Deadline,
    ) -> WinwrightResult<Bgra> {
        if let Some(item) = self.items.take(source, bounds) {
            match self.wgc.grab(&item, crop, deadline) {
                Ok(image) => {
                    self.items.put(source, bounds, item);
                    return Ok(image);
                }
                Err(WinwrightError::Cancelled) => return Err(WinwrightError::Cancelled),
                Err(e) => tracing::debug!(%e, "kept capture item failed; making a new one"),
            }
        }
        let item = source.item(&self.wgc)?;
        let image = self.wgc.grab(&item, crop, deadline)?;
        self.items.put(source, bounds, item);
        Ok(image)
    }

    fn monitor(
        &mut self,
        index: u32,
        deadline: &Deadline,
    ) -> WinwrightResult<(Bgra, PhysicalPoint)> {
        let monitors = monitors::enumerate()?;
        let monitor = monitors.get(index as usize).ok_or_else(|| {
            WinwrightError::invalid(format!(
                "monitor index {index} is out of range ({} monitors attached)",
                monitors.len()
            ))
        })?;
        let bounds = monitor.info.bounds;
        let source = Source::Monitor(monitor.handle.0 as usize);
        let image = self.grab(source, bounds, None, deadline)?;
        Ok((image, top_left(&bounds)))
    }

    /// Captures every monitor intersecting `rect` (GPU-cropped to the intersection) and
    /// composes them into one image covering exactly `rect`; uncovered pixels stay black.
    fn region(
        &mut self,
        rect: PhysicalRect,
        monitors: &[Monitor],
        deadline: &Deadline,
    ) -> WinwrightResult<Bgra> {
        let (width, height) = region_size(&rect)?;
        let bounds: Vec<PhysicalRect> = monitors.iter().map(|m| m.info.bounds).collect();
        let tiles = plan_region(&rect, &bounds);
        if tiles.is_empty() {
            return Err(WinwrightError::invalid(format!(
                "capture region [{}, {}, {}, {}] does not intersect any monitor",
                rect.left, rect.top, rect.right, rect.bottom
            )));
        }
        let mut canvas: Option<Bgra> = None;
        for tile in &tiles {
            deadline.check("capture region")?;
            let monitor = &monitors[tile.source];
            let source = Source::Monitor(monitor.handle.0 as usize);
            let part = self.grab(source, monitor.info.bounds, Some(tile.crop), deadline)?;
            if (part.width, part.height, tile.dst_x, tile.dst_y) == (width, height, 0, 0) {
                return Ok(part); // One monitor covers the whole region: no composition.
            }
            if canvas.is_none() {
                canvas = Some(Bgra::black(width, height)?);
            }
            if let Some(canvas) = canvas.as_mut() {
                blit(canvas, &part, tile.dst_x, tile.dst_y);
            }
        }
        canvas.ok_or_else(|| WinwrightError::CaptureFailed {
            reason: "no monitor produced pixels for the region".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_deadline_reports_the_real_elapsed_time() {
        let started = Instant::now() - std::time::Duration::from_millis(250);
        let deadline = Deadline {
            started,
            at: Instant::now(),
            cancel: CancellationToken::new(),
        };
        let Err(WinwrightError::Timeout { elapsed_ms, .. }) = deadline.check("capture") else {
            panic!("an expired deadline must time out");
        };
        assert!(elapsed_ms >= 250, "elapsed_ms={elapsed_ms}");
    }
}
