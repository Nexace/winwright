//! Caller sessions (spec §23). A session owns its refs, snapshot generations, and a
//! cancellation token. Cancelling is terminal: later operations fail with `CANCELLED` until the
//! user explicitly resets the session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio_util::sync::CancellationToken;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::ids::SessionId;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::refs::RefTable;

#[derive(Default)]
pub struct SessionState {
    pub generation: u64,
    pub refs: RefTable,
    /// Last rendered snapshot per window set, for diffs: key -> (generation, lines).
    pub snapshots: HashMap<String, (u64, Vec<crate::diff::Line>)>,
}

pub struct Session {
    pub id: SessionId,
    pub owner: String,
    pub started_at: SystemTime,
    cancel: CancellationToken,
    state: Mutex<SessionState>,
}

impl Session {
    fn new(id: SessionId, owner: String, parent: &CancellationToken) -> Self {
        Self {
            id,
            owner,
            started_at: SystemTime::now(),
            cancel: parent.child_token(),
            state: Mutex::default(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Context for one operation. Fails immediately if the session was cancelled.
    pub fn operation(&self, timeout: Duration) -> WinwrightResult<OperationContext> {
        if self.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        Ok(OperationContext::new(
            self.id.clone(),
            timeout,
            self.cancel.child_token(),
        ))
    }

    /// Never hold this guard across an `.await`.
    pub fn state(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct SessionRegistry {
    /// Parent of every session token: cancelling it is the emergency stop.
    root: Mutex<CancellationToken>,
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            root: Mutex::new(CancellationToken::new()),
            sessions: Mutex::default(),
        }
    }
}

impl SessionRegistry {
    pub const MAX_SESSIONS: usize = 64;

    fn sessions(&self) -> MutexGuard<'_, HashMap<SessionId, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn root(&self) -> CancellationToken {
        self.root
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn get_or_create(&self, id: &SessionId, owner: &str) -> WinwrightResult<Arc<Session>> {
        let root = self.root();
        if root.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        let mut sessions = self.sessions();
        if let Some(existing) = sessions.get(id) {
            return Ok(Arc::clone(existing));
        }
        if sessions.len() >= Self::MAX_SESSIONS {
            return Err(WinwrightError::invalid(format!(
                "too many sessions (max {})",
                Self::MAX_SESSIONS
            )));
        }
        let session = Arc::new(Session::new(id.clone(), owner.to_owned(), &root));
        sessions.insert(id.clone(), Arc::clone(&session));
        Ok(session)
    }

    pub fn get(&self, id: &SessionId) -> Option<Arc<Session>> {
        self.sessions().get(id).cloned()
    }

    /// Replaces a (possibly cancelled) session with a fresh one. User-initiated only.
    pub fn reset(&self, id: &SessionId) -> Option<Arc<Session>> {
        let old = self.sessions().remove(id);
        if let Some(old) = &old {
            old.cancel();
        }
        old
    }

    /// Emergency stop: cancels every current and future session in this engine.
    pub fn cancel_all(&self) {
        self.root().cancel();
    }

    pub fn is_stopped(&self) -> bool {
        self.root().is_cancelled()
    }

    /// Re-enables the registry after an emergency stop. Old sessions stay cancelled and are
    /// dropped; callers start fresh sessions. User-initiated only.
    pub fn rearm(&self) {
        let mut root = self.root.lock().unwrap_or_else(PoisonError::into_inner);
        if root.is_cancelled() {
            *root = CancellationToken::new();
            self.sessions().clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> SessionId {
        SessionId::parse(s).unwrap()
    }

    #[test]
    fn sessions_are_reused_by_id() {
        let reg = SessionRegistry::default();
        let a1 = reg.get_or_create(&sid("a"), "cli").unwrap();
        let a2 = reg.get_or_create(&sid("a"), "mcp").unwrap();
        assert!(Arc::ptr_eq(&a1, &a2));
        assert_eq!(a2.owner, "cli");
    }

    #[test]
    fn cancel_is_terminal_until_reset() {
        let reg = SessionRegistry::default();
        let s = reg.get_or_create(&sid("a"), "cli").unwrap();
        let ctx = s.operation(Duration::from_secs(5)).unwrap();
        s.cancel();
        assert!(
            ctx.cancel.is_cancelled(),
            "in-flight operations observe the cancel"
        );
        assert_eq!(
            s.operation(Duration::from_secs(5))
                .unwrap_err()
                .code()
                .as_str(),
            "CANCELLED"
        );
        reg.reset(&sid("a"));
        let fresh = reg.get_or_create(&sid("a"), "cli").unwrap();
        assert!(fresh.operation(Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn emergency_stop_cancels_everything() {
        let reg = SessionRegistry::default();
        let a = reg.get_or_create(&sid("a"), "cli").unwrap();
        let b = reg.get_or_create(&sid("b"), "mcp").unwrap();
        let ctx = b.operation(Duration::from_secs(5)).unwrap();
        reg.cancel_all();
        assert!(a.is_cancelled() && b.is_cancelled() && ctx.cancel.is_cancelled());
        assert!(reg.get_or_create(&sid("c"), "cli").is_err());
        assert!(reg.is_stopped());
        reg.rearm();
        assert!(!reg.is_stopped());
        let fresh = reg.get_or_create(&sid("a"), "cli").unwrap();
        assert!(!fresh.is_cancelled(), "rearm starts clean sessions");
        assert!(a.is_cancelled(), "old sessions stay cancelled");
    }

    #[test]
    fn context_check_reports_timeout_and_cancel() {
        let reg = SessionRegistry::default();
        let s = reg.get_or_create(&sid("a"), "cli").unwrap();
        let ctx = s.operation(Duration::ZERO).unwrap();
        assert_eq!(
            ctx.check("snapshot").unwrap_err().code().as_str(),
            "TIMEOUT"
        );
        let ctx = s.operation(Duration::from_secs(5)).unwrap();
        ctx.check("snapshot").unwrap();
        ctx.cancel.cancel();
        assert_eq!(
            ctx.check("snapshot").unwrap_err().code().as_str(),
            "CANCELLED"
        );
    }
}
