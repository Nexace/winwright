//! Real-desktop tests. Opt-in:
//! `cargo test -p winwright-capture -- --ignored --test-threads=1` on an unlocked session.
//!
//! Privacy: these tests only ever read pixels of a solid-color window they create themselves
//! (or a rectangle fully inside it). They never capture other windows, whole monitors, or the
//! desktop, never inject input, and never write images anywhere.

use std::ffi::c_void;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GA_ROOT, GetAncestor, GetMessageW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, SHOW_WINDOW_CMD, SW_SHOWMINNOACTIVE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_POPUP,
    WindowFromPoint,
};
use windows::core::w;
use winwright_capture::{WgcCapture, decode_bgra};
use winwright_contracts::backend::OperationContext;
use winwright_contracts::capture::{
    CaptureRequest, CaptureService, CaptureTarget, CapturedImage, ImageFormat,
};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::ids::SessionId;

/// RGB(32, 128, 192) as BGRA bytes.
const BGRA: [u8; 4] = [192, 128, 32, 255];
const WIDTH: i32 = 240;
const HEIGHT: i32 = 160;

fn ctx(timeout: Duration) -> OperationContext {
    OperationContext::new(
        SessionId::parse("live").unwrap(),
        timeout,
        CancellationToken::new(),
    )
}

fn request(target: CaptureTarget, format: ImageFormat) -> CaptureRequest {
    CaptureRequest {
        target,
        format,
        quality: 90,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        // SAFETY: ends this window thread's message loop.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: default handling for a window owned by this thread.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A borderless, topmost, non-activating popup filled with [`BGRA`], owned by its own
/// message-loop thread and destroyed on drop.
struct TestWindow {
    hwnd: u64,
    rect: PhysicalRect,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    fn open(origin: PhysicalPoint, show: SHOW_WINDOW_CMD) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            // SAFETY: standard window creation and message loop on this thread; every
            // pointer passed is a live local.
            unsafe {
                let instance = GetModuleHandleW(None).unwrap();
                let class = w!("WinwrightCaptureTestWindow");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: instance.into(),
                    lpszClassName: class,
                    hbrBackground: CreateSolidBrush(COLORREF(
                        u32::from(BGRA[2]) | u32::from(BGRA[1]) << 8 | u32::from(BGRA[0]) << 16,
                    )),
                    ..Default::default()
                };
                // Fails harmlessly when an earlier test already registered the class.
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    class,
                    w!("Winwright capture test"),
                    WS_POPUP,
                    origin.x,
                    origin.y,
                    WIDTH,
                    HEIGHT,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
                .unwrap();
                let _ = ShowWindow(hwnd, show);
                let _ = UpdateWindow(hwnd);
                let _ = DwmFlush();
                tx.send(hwnd.0 as usize as u64).unwrap();
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });
        let hwnd = rx.recv().unwrap();
        Self {
            hwnd,
            rect: PhysicalRect::new(origin.x, origin.y, origin.x + WIDTH, origin.y + HEIGHT),
            thread: Some(thread),
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        let hwnd = HWND(self.hwnd as usize as *mut c_void);
        // SAFETY: asks our own window thread to destroy the window and exit its loop.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the backend and lists candidate test-window origins inside the primary monitor's
/// work area: the four corners (inset) and the center.
fn start() -> (WgcCapture, [PhysicalPoint; 5]) {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let capture = WgcCapture::start().unwrap();
    let wa = capture
        .monitors()
        .unwrap()
        .into_iter()
        .find(|m| m.primary)
        .unwrap()
        .work_area;
    let (left, top) = (wa.left + 80, wa.top + 80);
    let (right, bottom) = (wa.right - WIDTH - 80, wa.bottom - HEIGHT - 80);
    let spot = |x, y| PhysicalPoint { x, y };
    let center = spot(
        wa.left + (wa.width() - WIDTH) / 2,
        wa.top + (wa.height() - HEIGHT) / 2,
    );
    let spots = [
        spot(left, top),
        spot(right, top),
        spot(left, bottom),
        spot(right, bottom),
        center,
    ];
    (capture, spots)
}

/// For window capture, where occlusion does not matter.
fn setup(show: SHOW_WINDOW_CMD) -> (WgcCapture, TestWindow) {
    let (capture, spots) = start();
    (capture, TestWindow::open(spots[0], show))
}

/// True when our window is the topmost window at its corners and center. Region capture reads
/// the monitor, so anything covering the test window (another app's topmost window) would be
/// read instead; this check lets the test avoid capturing anything but its own pixels.
fn is_uncovered(window: &TestWindow) -> bool {
    let r = window.rect;
    let c = r.center();
    [
        (r.left + 2, r.top + 2),
        (r.right - 3, r.top + 2),
        (r.left + 2, r.bottom - 3),
        (r.right - 3, r.bottom - 3),
        (c.x, c.y),
    ]
    .into_iter()
    .all(|(x, y)| {
        // SAFETY: read-only hit test and ancestor lookup.
        let root = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
        root.0 as usize as u64 == window.hwnd
    })
}

/// For monitor-based (region) capture: the first spot where nothing covers the test window.
/// Nothing is captured while searching.
fn setup_uncovered() -> (WgcCapture, TestWindow) {
    let (capture, spots) = start();
    for (i, spot) in spots.into_iter().enumerate() {
        let window = TestWindow::open(spot, SW_SHOWNOACTIVATE);
        if is_uncovered(&window) {
            return (capture, window);
        }
        eprintln!("test-window spot {i} is covered by another window; trying the next");
    }
    panic!("other topmost windows cover every test-window spot; region capture not validated");
}

fn decode(image: &CapturedImage) -> (u32, u32, Vec<u8>) {
    let (w, h, px) = decode_bgra(&image.bytes).unwrap();
    assert_eq!((w, h), (image.width, image.height));
    (w, h, px)
}

fn assert_solid(pixels: &[u8], tolerance: u8) {
    let differs =
        |px: &&[u8; 4]| (0..3).any(|i| px[i].abs_diff(BGRA[i]) > tolerance) || px[3] != 255;
    let px = pixels.as_chunks::<4>().0;
    let off = px.iter().filter(differs).count();
    let first = px.iter().find(differs);
    // Uniform black in a region capture means the monitor showed something other than the
    // test window there (a click-through overlay, a secure desktop, a display change).
    let black = px.iter().filter(|p| p[..3] == [0, 0, 0]).count();
    assert_eq!(
        off,
        0,
        "{off} of {} pixels differ from the test color ({black} black), first {first:?}",
        px.len()
    );
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn monitors_are_ordered_and_consistent() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let capture = WgcCapture::start().unwrap();
    let monitors = capture.monitors().unwrap();
    assert!(!monitors.is_empty());
    assert!(monitors[0].primary, "primary monitor comes first");
    assert_eq!(monitors.iter().filter(|m| m.primary).count(), 1);
    for (i, m) in monitors.iter().enumerate() {
        assert_eq!(m.index, i as u32);
        assert!(m.name.starts_with(r"\\.\"), "device name {:?}", m.name);
        assert!(!m.bounds.is_empty());
        assert!(m.work_area.left >= m.bounds.left && m.work_area.right <= m.bounds.right);
        assert!(m.dpi >= 96);
        assert_eq!(m.scale_percent, m.dpi * 100 / 96);
    }
    for pair in monitors[1..].windows(2) {
        let (a, b) = (&pair[0].bounds, &pair[1].bounds);
        assert!(
            (a.left, a.top) <= (b.left, b.top),
            "secondaries sorted by left, top"
        );
    }
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn window_capture_returns_the_windows_pixels() {
    let (capture, window) = setup(SW_SHOWNOACTIVATE);
    let mut timings = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let image = capture
            .capture(
                request(CaptureTarget::Window(window.hwnd), ImageFormat::Png),
                &ctx(Duration::from_secs(10)),
            )
            .await
            .unwrap();
        timings.push(started.elapsed().as_millis());
        assert_eq!(image.format, ImageFormat::Png);
        assert_eq!((image.width as i32, image.height as i32), (WIDTH, HEIGHT));
        assert_eq!(image.bounds(), window.rect);
        assert!(image.dpi >= 96);
        assert!(image.timestamp_ms > 0);
        let (_, _, pixels) = decode(&image);
        assert_solid(&pixels, 2);
    }
    println!("window capture (PNG, {WIDTH}x{HEIGHT}) timings: {timings:?} ms");
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn a_resized_window_is_captured_at_its_new_size() {
    let (capture, window) = setup(SW_SHOWNOACTIVATE);
    let ctx = ctx(Duration::from_secs(10));
    let grab = || {
        capture.capture(
            request(CaptureTarget::Window(window.hwnd), ImageFormat::Png),
            &ctx,
        )
    };
    let first = grab().await.unwrap();
    assert_eq!((first.width as i32, first.height as i32), (WIDTH, HEIGHT));
    let (w, h) = (WIDTH + 60, HEIGHT + 40);
    // SAFETY: resizes our own test window; no activation or z-order change.
    unsafe {
        SetWindowPos(
            HWND(window.hwnd as usize as *mut c_void),
            None,
            0,
            0,
            w,
            h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .unwrap();
        let _ = DwmFlush();
    }
    // The kept capture item was made for the old size: a new one must be used.
    let resized = grab().await.unwrap();
    assert_eq!((resized.width as i32, resized.height as i32), (w, h));
    let (_, _, pixels) = decode(&resized);
    assert_solid(&pixels, 2);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn window_capture_as_jpeg() {
    let (capture, window) = setup(SW_SHOWNOACTIVATE);
    let started = Instant::now();
    let image = capture
        .capture(
            request(CaptureTarget::Window(window.hwnd), ImageFormat::Jpeg),
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    println!(
        "window capture (JPEG): {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(image.format, ImageFormat::Jpeg);
    assert_eq!(&image.bytes[..2], &[0xFF, 0xD8]);
    let (_, _, pixels) = decode(&image);
    assert_solid(&pixels, 8);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn region_inside_the_test_window_is_cropped_exactly() {
    let (capture, window) = setup_uncovered();
    let r = window.rect;
    let region = PhysicalRect::new(r.left + 40, r.top + 30, r.left + 140, r.top + 90);
    let started = Instant::now();
    let image = capture
        .capture(
            request(CaptureTarget::Region(region), ImageFormat::Png),
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    println!(
        "region capture (100x60): {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(image.bounds(), region);
    let (w, h, pixels) = decode(&image);
    assert_eq!((w, h), (100, 60));
    assert_solid(&pixels, 2);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn minimized_window_is_a_typed_error() {
    let (capture, window) = setup(SW_SHOWMINNOACTIVE);
    let err = capture
        .capture(
            request(CaptureTarget::Window(window.hwnd), ImageFormat::Png),
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CAPTURE_FAILED");
    assert!(err.to_string().contains("minimized"), "{err}");
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn invalid_requests_fail_without_capturing() {
    let capture = WgcCapture::start().unwrap();
    let c = ctx(Duration::from_secs(10));
    let cases = [
        (CaptureTarget::Window(0xDEAD_0000), "WINDOW_NOT_FOUND"),
        (CaptureTarget::Monitor(u32::MAX), "INVALID_REQUEST"),
        (
            CaptureTarget::Region(PhysicalRect::new(10, 10, 10, 50)),
            "INVALID_REQUEST",
        ),
        (
            CaptureTarget::Region(PhysicalRect::new(0, 0, 20_000, 10)),
            "CAPTURE_FAILED",
        ),
    ];
    for (target, code) in cases {
        let err = capture
            .capture(request(target, ImageFormat::Png), &c)
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), code, "{target:?}: {err}");
    }
    let cancelled = ctx(Duration::from_secs(10));
    cancelled.cancel.cancel();
    let err = capture
        .capture(
            request(CaptureTarget::Monitor(0), ImageFormat::Png),
            &cancelled,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CANCELLED");
}
