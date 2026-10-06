//! One HTTPS request through WinHTTP, so no HTTP or TLS crates come in. Used for the Notion
//! copy and the tray's optional update check.

use std::ffi::c_void;

use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Closes a WinHTTP handle when dropped.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle WinHTTP returned, closed once.
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// An HTTPS `verb` to `host` and `path` with extra `headers` (`Name: value\r\n` lines) and a
/// body: the status code and the response body (at most about 1 MB). The user agent is
/// "Winwright". Errors name the host and the failing step, never the headers.
pub fn request(
    verb: &str,
    host: &str,
    path: &str,
    headers: &str,
    body: &[u8],
) -> Result<(u32, Vec<u8>), String> {
    let fail =
        |what: &str, err: windows::core::Error| format!("cannot reach {host} ({what}: {err})");
    let handle = |raw: *mut c_void, what: &str| {
        if raw.is_null() {
            Err(fail(what, windows::core::Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    };
    let verb = HSTRING::from(verb);
    let server = HSTRING::from(host);
    let path = HSTRING::from(path);
    let headers: Vec<u16> = headers.encode_utf16().collect();
    let length = u32::try_from(body.len()).map_err(|_| "request too large".to_owned())?;
    // SAFETY: plain WinHTTP calls in order; every handle is closed by its guard, every buffer
    // outlives the call that uses it, and lengths match the buffers passed.
    unsafe {
        let session = handle(
            WinHttpOpen(
                w!("Winwright"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "open",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 15_000)
            .map_err(|e| fail("timeouts", e))?;
        let connect = handle(
            WinHttpConnect(session.0, &server, INTERNET_DEFAULT_HTTPS_PORT, 0),
            "connect",
        )?;
        let request = handle(
            WinHttpOpenRequest(
                connect.0,
                &verb,
                &path,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "request",
        )?;
        WinHttpSendRequest(
            request.0,
            (!headers.is_empty()).then_some(headers.as_slice()),
            (!body.is_empty()).then_some(body.as_ptr().cast()),
            length,
            length,
            0,
        )
        .map_err(|e| fail("send", e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| fail("response", e))?;
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| fail("status", e))?;
        let mut response = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| fail("read", e))?;
            if read == 0 || response.len() > 1 << 20 {
                break;
            }
            response.extend_from_slice(&chunk[..read as usize]);
        }
        Ok((status, response))
    }
}
