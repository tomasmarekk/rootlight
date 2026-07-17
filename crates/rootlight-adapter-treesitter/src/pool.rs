//! Cancellation-aware bounded pool of reusable Tree-sitter parsers.
//!
//! A lease resets parser continuation state and returns its permit in `Drop`,
//! including provider, sink, timeout, and cancellation error paths.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use rootlight_cancel::Cancellation;
use tree_sitter::Parser;

const PERMIT_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) struct ParserPool {
    inner: Arc<PoolInner>,
}

impl ParserPool {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                capacity,
                state: Mutex::new(PoolState {
                    available: Vec::with_capacity(capacity),
                    created: 0,
                    checked_out: 0,
                }),
                available: Condvar::new(),
            }),
        }
    }

    pub(crate) fn acquire(&self, cancellation: &Cancellation) -> Result<ParserLease, PoolError> {
        let mut state = self.inner.state.lock().map_err(|_| PoolError::Poisoned)?;
        loop {
            cancellation.check().map_err(PoolError::Cancelled)?;
            if let Some(parser) = state.available.pop() {
                state.checked_out = state
                    .checked_out
                    .checked_add(1)
                    .ok_or(PoolError::AccountingOverflow)?;
                return Ok(ParserLease {
                    parser: Some(parser),
                    inner: Arc::clone(&self.inner),
                });
            }
            if state.created < self.inner.capacity {
                state.created = state
                    .created
                    .checked_add(1)
                    .ok_or(PoolError::AccountingOverflow)?;
                state.checked_out = state
                    .checked_out
                    .checked_add(1)
                    .ok_or(PoolError::AccountingOverflow)?;
                return Ok(ParserLease {
                    parser: Some(Parser::new()),
                    inner: Arc::clone(&self.inner),
                });
            }
            let (next, _) = self
                .inner
                .available
                .wait_timeout(state, PERMIT_POLL_INTERVAL)
                .map_err(|_| PoolError::Poisoned)?;
            state = next;
        }
    }

    pub(crate) fn stats(&self) -> PoolStats {
        let state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        PoolStats {
            created: state.created,
            available: state.available.len(),
            checked_out: state.checked_out,
        }
    }
}

pub(crate) struct ParserLease {
    parser: Option<Parser>,
    inner: Arc<PoolInner>,
}

impl ParserLease {
    pub(crate) fn parser_mut(&mut self) -> Result<&mut Parser, PoolError> {
        self.parser.as_mut().ok_or(PoolError::MissingParser)
    }
}

impl Drop for ParserLease {
    fn drop(&mut self) {
        let Some(mut parser) = self.parser.take() else {
            return;
        };
        parser.reset();
        let _ = parser.set_included_ranges(&[]);
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.checked_out = state.checked_out.saturating_sub(1);
        state.available.push(parser);
        drop(state);
        self.inner.available.notify_one();
    }
}

struct PoolInner {
    capacity: usize,
    state: Mutex<PoolState>,
    available: Condvar,
}

struct PoolState {
    available: Vec<Parser>,
    created: usize,
    checked_out: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PoolStats {
    pub(crate) created: usize,
    pub(crate) available: usize,
    pub(crate) checked_out: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PoolError {
    #[error("parser pool was cancelled")]
    Cancelled(#[source] rootlight_cancel::Cancelled),
    #[error("parser pool accounting overflowed")]
    AccountingOverflow,
    #[error("parser pool synchronization failed")]
    Poisoned,
    #[error("parser lease lost its parser")]
    MissingParser,
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_waiting_acquire_observes_its_deadline_and_preserves_the_permit() {
        let pool = ParserPool::new(1);
        let lease = pool
            .acquire(&deadline(Duration::from_secs(1)))
            .expect("first permit is available");

        assert!(matches!(
            pool.acquire(&deadline(Duration::from_millis(20))),
            Err(PoolError::Cancelled(_))
        ));
        assert_eq!(pool.stats().checked_out, 1);

        drop(lease);
        let stats = pool.stats();
        assert_eq!(stats.created, 1);
        assert_eq!(stats.available, 1);
        assert_eq!(stats.checked_out, 0);
    }

    fn deadline(duration: Duration) -> Cancellation {
        Cancellation::with_deadline(
            Instant::now()
                .checked_add(duration)
                .expect("test deadline is representable"),
        )
    }
}
