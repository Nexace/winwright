//! Trusted local confirmation dialog (spec §66). A native Windows message box that only the
//! person at the keyboard can answer: "No" is the default button, and a prompt that is not
//! answered in time, or whose request is abandoned, is closed with "No".

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IDNO, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MessageBoxW, PostMessageW, WM_COMMAND,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::backend::BackendFuture;
use winwright_contracts::security::{ConfirmationPrompt, Confirmer};

static NEXT_PROMPT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default)]
pub struct NativeConfirmer;

impl NativeConfirmer {
    pub fn new() -> Self {
        Self
    }
}

/// The dialog text. Pure so it can be tested; never contains typed text or field values
/// (the engine's summaries only count characters).
pub fn dialog_text(prompt: &ConfirmationPrompt) -> String {
    let mut text = format!(
        "An AI assistant using Winwright wants to:\n\n    {}\n",
        prompt.summary
    );
    if let Some(t) = &prompt.target {
        if let Some(window) = &t.window {
            text.push_str(&format!("\nWindow: {window}"));
        }
        if let Some(process) = &t.process {
            text.push_str(&format!("\nApp: {process}"));
        }
        text.push('\n');
    }
    text.push_str(&format!(
        "\nWhy you are asked: {}\n\nAllow this once?\n\n\
         \"No\" is the default. Unanswered requests are denied after {} seconds.\n\
         Press Ctrl+Alt+Esc at any time to stop Winwright.",
        prompt.reason,
        prompt.timeout_ms / 1000
    ));
    text
}

/// Answers an abandoned or timed-out dialog with "No" so it never lingers.
struct DenyOnDrop {
    title: HSTRING,
}

impl Drop for DenyOnDrop {
    fn drop(&mut self) {
        // SAFETY: plain window lookup and message post; both take owned/borrowed values only.
        unsafe {
            if let Ok(hwnd) = FindWindowW(w!("#32770"), &self.title) {
                let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(IDNO.0 as usize), LPARAM(0));
            }
        }
    }
}

impl Confirmer for NativeConfirmer {
    fn confirm<'a>(&'a self, prompt: ConfirmationPrompt) -> BackendFuture<'a, bool> {
        let id = NEXT_PROMPT.fetch_add(1, Ordering::Relaxed);
        let title = HSTRING::from(format!(
            "Winwright: confirm action ({}-{id})",
            std::process::id()
        ));
        let text = HSTRING::from(dialog_text(&prompt));
        let timeout = Duration::from_millis(prompt.timeout_ms.max(1_000));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let thread_title = title.clone();
        let spawned = std::thread::Builder::new()
            .name("winwright-confirm".into())
            .spawn(move || {
                // SAFETY: both strings outlive the modal call on this thread.
                let answer = unsafe {
                    MessageBoxW(
                        None,
                        PCWSTR(text.as_ptr()),
                        PCWSTR(thread_title.as_ptr()),
                        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND,
                    )
                };
                let _ = tx.send(answer == IDYES);
            });
        Box::pin(async move {
            if spawned.is_err() {
                return Ok(false);
            }
            let _deny_on_drop = DenyOnDrop { title };
            Ok(tokio::time::timeout(timeout, rx)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(false))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::security::TargetSummary;

    #[test]
    fn dialog_names_the_action_window_and_reason() {
        let text = dialog_text(&ConfirmationPrompt {
            summary: "Click Button \"Send\"".into(),
            target: Some(TargetSummary {
                process: Some("outlook.exe".into()),
                window: Some("Inbox - Outlook".into()),
                role: Some("Button".into()),
                name: Some("Send".into()),
            }),
            reason: "action may send, submit, delete, or spend".into(),
            timeout_ms: 60_000,
        });
        assert!(text.contains("Click Button \"Send\""));
        assert!(text.contains("Window: Inbox - Outlook"));
        assert!(text.contains("App: outlook.exe"));
        assert!(text.contains("denied after 60 seconds"));
    }
}
