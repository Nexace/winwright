//! Raw UI Automation COM behind a dedicated MTA worker (spec §5).
//!
//! [`UiaBackend`] is the thread-safe mailbox proxy: Tokio tasks send owned commands over a
//! bounded channel and await oneshot replies. No COM interface ever leaves the worker thread.

mod com;
mod events;
mod patterns;
mod props;
mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use winwright_contracts::backend::{
    BackendFuture, ElementKey, EventSubscription, InspectTarget, OperationContext, UiActionOutcome,
    UiAutomationBackend, UiInspection, UiPatternAction, UiProps, UiTree, UiTreeRequest,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::worker::{Command, Deadline};

const QUEUE_DEPTH: usize = 32;

static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

pub struct UiaBackend {
    tx: mpsc::Sender<Command>,
    epoch: u64,
    events: tokio::sync::watch::Receiver<u64>,
    listeners: Arc<AtomicUsize>,
}

fn unavailable() -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: "UIAutomation".into(),
        reason: "the automation worker stopped".into(),
    }
}

impl UiaBackend {
    /// Spawns the worker thread and waits until COM and `CUIAutomation` are ready.
    pub fn start() -> WinwrightResult<Self> {
        let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(QUEUE_DEPTH);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (events_tx, events) = tokio::sync::watch::channel(0u64);
        let listeners = Arc::new(AtomicUsize::new(0));
        let event_state = worker::EventState::new(Arc::new(events_tx), Arc::clone(&listeners));
        std::thread::Builder::new()
            .name(format!("winwright-uia-{epoch}"))
            .stack_size(8 * 1024 * 1024)
            .spawn(move || worker::run(epoch, rx, ready_tx, event_state))
            .map_err(|e| WinwrightError::BackendUnavailable {
                backend: "UIAutomation".into(),
                reason: format!("cannot spawn worker thread: {e}"),
            })?;
        ready_rx.recv().map_err(|_| unavailable())??;
        Ok(Self {
            tx,
            epoch,
            events,
            listeners,
        })
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

/// Once a mutating command reached the worker, a timeout cannot prove it did not run.
fn outcome_unknown_on_timeout<T>(
    operation: &str,
    result: WinwrightResult<T>,
) -> WinwrightResult<T> {
    match result {
        Err(WinwrightError::Timeout { elapsed_ms, .. }) if elapsed_ms > 0 => {
            Err(WinwrightError::ActionOutcomeUnknown {
                operation: operation.to_owned(),
                reason: format!("no reply after {elapsed_ms} ms; the action may have run"),
            })
        }
        other => other,
    }
}

impl UiAutomationBackend for UiaBackend {
    fn worker_epoch(&self) -> u64 {
        self.epoch
    }

    fn events(&self) -> Option<EventSubscription> {
        self.listeners.fetch_add(1, Ordering::AcqRel);
        let _ = self.tx.try_send(Command::SyncEvents);
        let listeners = Arc::clone(&self.listeners);
        let tx = self.tx.clone();
        Some(EventSubscription::new(self.events.clone(), move || {
            listeners.fetch_sub(1, Ordering::AcqRel);
            // A lost message is harmless: the worker reconciles after every command.
            let _ = tx.try_send(Command::SyncEvents);
        }))
    }

    fn capture_tree<'a>(
        &'a self,
        request: UiTreeRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiTree> {
        Box::pin(self.call(ctx, "capture_tree", move |deadline, reply| {
            Command::CaptureTree {
                request,
                deadline,
                reply,
            }
        }))
    }

    fn inspect<'a>(
        &'a self,
        target: InspectTarget,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiInspection> {
        Box::pin(
            self.call(ctx, "inspect", move |deadline, reply| Command::Inspect {
                target,
                deadline,
                reply,
            }),
        )
    }

    fn release<'a>(&'a self, keys: Vec<ElementKey>) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            if keys.is_empty() {
                return Ok(());
            }
            self.tx
                .send(Command::Release { keys })
                .await
                .map_err(|_| unavailable())
        })
    }

    fn refresh<'a>(
        &'a self,
        key: ElementKey,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiProps> {
        Box::pin(self.call(ctx, "refresh", move |_, reply| Command::Refresh {
            key,
            reply,
        }))
    }

    fn execute_pattern<'a>(
        &'a self,
        key: ElementKey,
        action: UiPatternAction,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiActionOutcome> {
        let mutating = !matches!(
            action,
            UiPatternAction::GetText { .. } | UiPatternAction::ClickablePoint
        );
        Box::pin(async move {
            let result = self
                .call(ctx, "execute_pattern", move |_, reply| Command::Execute {
                    key,
                    action,
                    reply,
                })
                .await;
            if mutating {
                outcome_unknown_on_timeout("execute_pattern", result)
            } else {
                result
            }
        })
    }
}
