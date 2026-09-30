//! Backends that start on first use, so an idle server holds no D3D device or capture thread.

use std::sync::{Arc, Mutex, PoisonError};

use winwright_capture::WgcCapture;
use winwright_contracts::WinwrightResult;
use winwright_contracts::backend::{BackendFuture, OperationContext};
use winwright_contracts::capture::{CaptureRequest, CaptureService, CapturedImage, MonitorInfo};

#[derive(Default)]
pub struct LazyCapture {
    inner: Mutex<Option<Arc<WgcCapture>>>,
}

impl LazyCapture {
    fn get(&self) -> WinwrightResult<Arc<WgcCapture>> {
        let mut slot = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(capture) = slot.as_ref() {
            return Ok(Arc::clone(capture));
        }
        let capture = Arc::new(WgcCapture::start()?);
        *slot = Some(Arc::clone(&capture));
        Ok(capture)
    }
}

impl CaptureService for LazyCapture {
    fn monitors(&self) -> WinwrightResult<Vec<MonitorInfo>> {
        self.get()?.monitors()
    }

    fn capture<'a>(
        &'a self,
        request: CaptureRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, CapturedImage> {
        Box::pin(async move {
            let capture = self.get()?;
            capture.capture(request, ctx).await
        })
    }
}
