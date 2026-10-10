//! Per-account request admission after credential verification and before database work.
use crate::error::AppError;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub(crate) struct RequestBudget(Arc<Mutex<HashMap<String, VecDeque<Instant>>>>);
impl RequestBudget {
    pub(crate) fn admit(&self, uuid: &str) -> Result<(), AppError> {
        let mut state = self.0.lock().map_err(|_| {
            AppError::internal("request_budget", anyhow::anyhow!("budget lock unavailable"))
        })?;
        let now = Instant::now();
        state.retain(|_, window| {
            window
                .back()
                .is_some_and(|at| now.duration_since(*at) < Duration::from_secs(60))
        });
        if state.len() >= 10000 && !state.contains_key(uuid) {
            return Err(limited());
        }
        let window = state.entry(uuid.to_owned()).or_default();
        while window
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            window.pop_front();
        }
        if window.len() >= 300 {
            return Err(limited());
        }
        window.push_back(now);
        Ok(())
    }
}
fn limited() -> AppError {
    AppError::RateLimited {
        retry_after_seconds: 60,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloned_budgets_share_a_callers_limit_without_blocking_another() -> anyhow::Result<()> {
        let first = RequestBudget::default();
        let second = first.clone();
        for _ in 0..150 {
            first.admit("Ada")?;
            second.admit("Ada")?;
        }
        assert!(first.admit("Ada").is_err());
        second.admit("Ian")?;
        Ok(())
    }
}
