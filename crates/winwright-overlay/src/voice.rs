//! Spoken guide steps: the built-in Windows voice through SAPI (`ISpVoice`), on its own thread
//! started on first use. Speech is asynchronous, so nothing ever waits for a sentence to end.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

use windows::Win32::Media::Speech::{
    ISpVoice, SPF_ASYNC, SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK, SpVoice,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::PCWSTR;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::lock;

/// Text to read, or `None` to stop.
type Line = Option<String>;

#[derive(Default)]
pub struct Voice {
    /// `None` until the first sentence, and again after the thread failed.
    sender: Mutex<Option<Sender<Line>>>,
}

impl Voice {
    pub fn speak(&self, text: &str) -> WinwrightResult<()> {
        let mut sender = lock(&self.sender);
        if sender.is_none() {
            *sender = Some(start()?);
        }
        if let Some(tx) = sender.as_ref()
            && tx.send(Some(text.to_owned())).is_ok()
        {
            return Ok(());
        }
        *sender = None;
        Err(WinwrightError::BackendUnavailable {
            backend: "voice".into(),
            reason: "the speech thread stopped".into(),
        })
    }

    pub fn hush(&self) {
        if let Some(tx) = lock(&self.sender).as_ref() {
            let _ = tx.send(None);
        }
    }
}

/// Starts the speech thread without waiting for it. When the voice cannot be made, the thread
/// ends and the next [`Voice::speak`] reports it.
fn start() -> WinwrightResult<Sender<Line>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("winwright-voice".into())
        .spawn(move || {
            // SAFETY: once on this thread, undone below.
            if let Err(err) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
                tracing::warn!(%err, "no COM for the Windows voice");
                return;
            }
            // SAFETY: COM is initialized on this thread.
            match unsafe { CoCreateInstance::<_, ISpVoice>(&SpVoice, None, CLSCTX_ALL) } {
                Ok(voice) => run(&voice, &rx),
                Err(err) => tracing::warn!(%err, "the Windows voice is unavailable"),
            }
            // SAFETY: pairs with the successful CoInitializeEx above; the voice is released.
            unsafe { CoUninitialize() };
        })
        .map_err(|e| WinwrightError::BackendUnavailable {
            backend: "voice".into(),
            reason: format!("cannot start the speech thread: {e}"),
        })?;
    Ok(tx)
}

/// Speaks each line as it comes, dropping what was still being said; stops when every handle
/// is gone.
fn run(voice: &ISpVoice, rx: &Receiver<Line>) {
    let purge = SPF_PURGEBEFORESPEAK.0 as u32;
    while let Ok(line) = rx.recv() {
        let result = match line {
            Some(text) => {
                let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
                let flags = (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0 | SPF_IS_NOT_XML.0) as u32;
                // SAFETY: `wide` is NUL-terminated; with SPF_ASYNC SAPI copies it before
                // returning.
                unsafe { voice.Speak(PCWSTR(wide.as_ptr()), flags, None) }
            }
            // SAFETY: no text with SPF_PURGEBEFORESPEAK only stops the current speech.
            None => unsafe { voice.Speak(PCWSTR::null(), purge, None) },
        };
        if let Err(err) = result {
            tracing::warn!(%err, "the Windows voice failed");
        }
    }
    // SAFETY: as above.
    let _ = unsafe { voice.Speak(PCWSTR::null(), purge, None) };
}
