//! Screen capture over Windows.Graphics.Capture behind a dedicated MTA worker (spec §18).
//!
//! [`WgcCapture`] is the thread-safe mailbox proxy: Tokio tasks send owned requests over a
//! bounded channel and await oneshot replies. The D3D11 device, frame pools, capture sessions,
//! and the WIC factory never leave the worker thread; callers only receive owned, encoded
//! [`CapturedImage`] bytes. Nothing is written to disk and pixel data is never logged (§69).
//!
//! Geometry is physical virtual-desktop pixels throughout. Capture frames are physical too, so
//! region crops translate desktop coordinates into capture-local pixels by subtracting the
//! capture origin and never scale again (§18 step 3, §43).

mod com;
mod marks;
mod monitors;
mod ocr;
mod raster;
mod wgc;
mod wic;
mod window;
mod worker;

use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use winwright_contracts::backend::{BackendFuture, OperationContext};
use winwright_contracts::capture::{
    CaptureRequest, CaptureService, CapturedImage, MonitorInfo, ScreenText,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::worker::{Command, Deadline};

const QUEUE_DEPTH: usize = 8;

fn unavailable() -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: "Windows.Graphics.Capture".into(),
        reason: "the capture worker stopped".into(),
    }
}

pub struct WgcCapture {
    tx: mpsc::Sender<Command>,
}

impl WgcCapture {
    /// Spawns the worker thread and waits until COM, D3D11, and WIC are ready. Fails with
    /// `BACKEND_UNAVAILABLE` when Windows.Graphics.Capture is not supported.
    pub fn start() -> WinwrightResult<Self> {
        let (tx, rx) = mpsc::channel(QUEUE_DEPTH);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("winwright-capture".into())
            .spawn(move || worker::run(rx, ready_tx))
            .map_err(|e| WinwrightError::BackendUnavailable {
                backend: "Windows.Graphics.Capture".into(),
                reason: format!("cannot spawn worker thread: {e}"),
            })?;
        ready_rx.recv().map_err(|_| unavailable())??;
        Ok(Self { tx })
    }

    async fn call<T>(
        &self,
        ctx: &OperationContext,
        operation: &'static str,
        make: impl FnOnce(Deadline, oneshot::Sender<WinwrightResult<T>>) -> Command,
    ) -> WinwrightResult<T> {
        ctx.check(operation)?;
        let started = Instant::now();
        let timeout = || WinwrightError::Timeout {
            operation: operation.into(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        let deadline_sleep = tokio::time::sleep_until(ctx.deadline.into());
        tokio::pin!(deadline_sleep);

        let (reply, rx) = oneshot::channel();
        let command = make(
            Deadline {
                started: ctx.started,
                at: ctx.deadline,
                cancel: ctx.cancel.clone(),
            },
            reply,
        );
        tokio::select! {
            sent = self.tx.send(command) => sent.map_err(|_| unavailable())?,
            () = ctx.cancel.cancelled() => return Err(WinwrightError::Cancelled),
            () = &mut deadline_sleep => return Err(timeout()),
        }
        tokio::select! {
            result = rx => result.map_err(|_| unavailable())?,
            () = ctx.cancel.cancelled() => Err(WinwrightError::Cancelled),
            () = &mut deadline_sleep => Err(timeout()),
        }
    }
}

impl CaptureService for WgcCapture {
    /// Runs on the calling thread: plain Win32, no COM.
    fn monitors(&self) -> WinwrightResult<Vec<MonitorInfo>> {
        Ok(monitors::enumerate()?.into_iter().map(|m| m.info).collect())
    }

    fn capture<'a>(
        &'a self,
        request: CaptureRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, CapturedImage> {
        Box::pin(
            self.call(ctx, "capture", move |deadline, reply| Command::Capture {
                request,
                deadline,
                reply,
            }),
        )
    }

    fn read_text<'a>(
        &'a self,
        request: CaptureRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ScreenText> {
        Box::pin(
            self.call(ctx, "read text", move |deadline, reply| Command::ReadText {
                request,
                deadline,
                reply,
            }),
        )
    }
}

/// Decodes a PNG or JPEG (anything WIC reads) into tightly packed BGRA8 pixels:
/// `(width, height, pixels)` with a stride of `width * 4`. For tests and vision code; runs on
/// the calling thread, joining the MTA unless the thread already has a COM apartment.
pub fn decode_bgra(bytes: &[u8]) -> WinwrightResult<(u32, u32, Vec<u8>)> {
    let _apartment = com::ComApartment::ensure()?;
    let image = wic::Wic::new()?.decode(bytes)?;
    Ok((image.width, image.height, image.pixels))
}
