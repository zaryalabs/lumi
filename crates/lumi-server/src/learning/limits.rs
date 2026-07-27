//! Payload-free rate and concurrency guards for expensive learning operations.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumi_core::UserId;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const RATE_WINDOW: Duration = Duration::from_secs(60);
const LEARNING_AI_REQUESTS_PER_WINDOW: usize = 30;
const TRANSCRIPTION_REQUESTS_PER_WINDOW: usize = 10;
const LEARNING_AI_CONCURRENCY: usize = 16;
const TRANSCRIPTION_CONCURRENCY: usize = 4;

/// Expensive learning operation family with an independent budget.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LearningOperationKind {
    /// Source-backed generation and evaluation requests.
    Ai,
    /// Audio transcription requests.
    Transcription,
}

/// Held for the lifetime of one admitted operation.
pub(crate) struct LearningOperationPermit {
    _permit: OwnedSemaphorePermit,
}

/// Admission failure safe to map to HTTP/MCP rate-limit responses.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum LearningOperationLimitError {
    /// The account exhausted its bounded fixed-window allowance.
    #[error("learning operation rate limit exceeded")]
    RateLimited,
    /// The process has no free execution slot for this operation family.
    #[error("learning operation concurrency limit exceeded")]
    Busy,
    /// The limiter mutex was poisoned.
    #[error("learning operation limiter is unavailable")]
    Unavailable,
}

/// Process-local admission controller; provider-side limits remain authoritative.
pub(crate) struct LearningOperationLimits {
    windows: Mutex<HashMap<(UserId, LearningOperationKind), VecDeque<Instant>>>,
    ai: Arc<Semaphore>,
    transcription: Arc<Semaphore>,
}

impl Default for LearningOperationLimits {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            ai: Arc::new(Semaphore::new(LEARNING_AI_CONCURRENCY)),
            transcription: Arc::new(Semaphore::new(TRANSCRIPTION_CONCURRENCY)),
        }
    }
}

impl LearningOperationLimits {
    /// Admit one account-scoped operation without waiting in an unbounded queue.
    pub(crate) fn acquire(
        &self,
        user_id: UserId,
        kind: LearningOperationKind,
    ) -> Result<LearningOperationPermit, LearningOperationLimitError> {
        let semaphore = match kind {
            LearningOperationKind::Ai => Arc::clone(&self.ai),
            LearningOperationKind::Transcription => Arc::clone(&self.transcription),
        };
        let permit = semaphore
            .try_acquire_owned()
            .map_err(|_| LearningOperationLimitError::Busy)?;
        let limit = match kind {
            LearningOperationKind::Ai => LEARNING_AI_REQUESTS_PER_WINDOW,
            LearningOperationKind::Transcription => TRANSCRIPTION_REQUESTS_PER_WINDOW,
        };
        let now = Instant::now();
        let mut windows = self
            .windows
            .lock()
            .map_err(|_| LearningOperationLimitError::Unavailable)?;
        let window = windows.entry((user_id, kind)).or_default();
        while window
            .front()
            .is_some_and(|created_at| now.duration_since(*created_at) >= RATE_WINDOW)
        {
            window.pop_front();
        }
        if window.len() >= limit {
            return Err(LearningOperationLimitError::RateLimited);
        }
        window.push_back(now);
        Ok(LearningOperationPermit { _permit: permit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_rate_windows_are_isolated() {
        let limits = LearningOperationLimits::default();
        let first = UserId::now_v7();
        let second = UserId::now_v7();
        for _ in 0..LEARNING_AI_REQUESTS_PER_WINDOW {
            drop(limits.acquire(first, LearningOperationKind::Ai));
        }

        assert!(matches!(
            limits.acquire(first, LearningOperationKind::Ai),
            Err(LearningOperationLimitError::RateLimited)
        ));
        assert!(limits.acquire(second, LearningOperationKind::Ai).is_ok());
    }
}
