//! UI Automation event subscriptions (spec §16). Handlers run on UIA's own threads, so they
//! only bump a counter: no UIA calls, no locks, no allocation. Waits treat a bump as "look
//! again", never as proof. Subscriptions are added and removed on the worker thread.

use tokio::sync::watch;
use windows::Win32::UI::Accessibility::*;
use windows::core::{Interface, Ref, implement};

#[implement(IUIAutomationEventHandler, IUIAutomationFocusChangedEventHandler)]
struct Notifier {
    tx: watch::Sender<u64>,
}

impl Notifier {
    fn bump(&self) {
        self.tx.send_modify(|n| *n = n.wrapping_add(1));
    }
}

impl IUIAutomationEventHandler_Impl for Notifier_Impl {
    fn HandleAutomationEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        _event: UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        self.bump();
        Ok(())
    }
}

impl IUIAutomationFocusChangedEventHandler_Impl for Notifier_Impl {
    fn HandleFocusChangedEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        self.bump();
        Ok(())
    }
}

/// Registers window-opened/closed (desktop subtree) and focus-changed handlers.
/// Failures are logged and leave waits on polling alone.
pub fn subscribe(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    tx: watch::Sender<u64>,
) -> bool {
    let notifier = Notifier { tx };
    let handler: IUIAutomationEventHandler = notifier.into();
    let Ok(focus) = handler.cast::<IUIAutomationFocusChangedEventHandler>() else {
        tracing::warn!("event notifier does not expose the focus interface");
        return false;
    };
    let mut ok = true;
    for event in [
        UIA_Window_WindowOpenedEventId,
        UIA_Window_WindowClosedEventId,
    ] {
        // SAFETY: COM call on interfaces owned by the worker thread; `handler` lives until
        // `RemoveAllEventHandlers` in `unsubscribe`.
        let added = unsafe {
            automation.AddAutomationEventHandler(
                event,
                root,
                TreeScope_Subtree,
                None::<&IUIAutomationCacheRequest>,
                &handler,
            )
        };
        if let Err(err) = added {
            tracing::warn!(%err, event = event.0, "could not subscribe to UIA window events");
            ok = false;
        }
    }
    // SAFETY: as above.
    if let Err(err) = unsafe {
        automation.AddFocusChangedEventHandler(None::<&IUIAutomationCacheRequest>, &focus)
    } {
        tracing::warn!(%err, "could not subscribe to UIA focus events");
        ok = false;
    }
    ok
}

/// Removes every handler this client registered. Must run on the worker before COM teardown.
pub fn unsubscribe(automation: &IUIAutomation) {
    // SAFETY: COM call on the worker thread that registered the handlers.
    if let Err(err) = unsafe { automation.RemoveAllEventHandlers() } {
        tracing::warn!(%err, "RemoveAllEventHandlers failed");
    }
}
