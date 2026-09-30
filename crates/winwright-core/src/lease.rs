//! Exclusive action lease (spec §45): one session at a time may change desktop state.
//! Inspection never takes the lease. Cross-process coordination arrives with `winwright serve`.

use std::sync::{Arc, Mutex, PoisonError};

use winwright_contracts::ids::SessionId;
use winwright_contracts::{WinwrightError, WinwrightResult};

#[derive(Clone, Default)]
pub struct ActionLease {
    holder: Arc<Mutex<Option<SessionId>>>,
}

/// Releases the lease on drop, including on panic or cancellation unwinding.
pub struct LeaseGuard {
    holder: Arc<Mutex<Option<SessionId>>>,
}

impl ActionLease {
    pub fn try_acquire(&self, session: &SessionId) -> WinwrightResult<LeaseGuard> {
        let mut holder = self.holder.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = holder.as_ref() {
            return Err(WinwrightError::DesktopBusy {
                holder: current.to_string(),
            });
        }
        *holder = Some(session.clone());
        Ok(LeaseGuard {
            holder: Arc::clone(&self.holder),
        })
    }

    pub fn holder(&self) -> Option<SessionId> {
        self.holder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        *self.holder.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_session_is_busy_until_release() {
        let lease = ActionLease::default();
        let a = SessionId::parse("a").unwrap();
        let b = SessionId::parse("b").unwrap();
        let guard = lease.try_acquire(&a).unwrap();
        let err = lease.try_acquire(&b).err().unwrap();
        assert_eq!(err.code().as_str(), "DESKTOP_BUSY");
        assert_eq!(lease.holder(), Some(a.clone()));
        drop(guard);
        assert_eq!(lease.holder(), None);
        let _guard = lease.try_acquire(&b).unwrap();
    }

    #[test]
    fn same_session_cannot_interleave_actions() {
        let lease = ActionLease::default();
        let a = SessionId::parse("a").unwrap();
        let _guard = lease.try_acquire(&a).unwrap();
        assert!(lease.try_acquire(&a).is_err());
    }
}
