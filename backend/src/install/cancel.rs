//! Cooperative cancellation for long installs: the work checks the token
//! between steps and races every network wait against it, so a stop takes
//! effect within one chunk and leaves no half-written files behind.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;

#[derive(Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Default for Cancel {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl Cancel {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once `cancel` is called (right away if it already was).
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        // The sender lives in `self`: `wait_for` can only end by matching.
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }

    /// `work`, unless the token fires first (`None`).
    pub async fn run<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            () = self.cancelled() => None,
            value = work => Some(value),
        }
    }

    /// Sleeps for `delay`; `false` when cancelled meanwhile.
    pub async fn sleep(&self, delay: std::time::Duration) -> bool {
        self.run(tokio::time::sleep(delay)).await.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn stops_work_and_sleeps() {
        let cancel = Cancel::default();
        assert!(!cancel.is_cancelled());
        assert_eq!(cancel.run(async { 7 }).await, Some(7));

        let clone = cancel.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            clone.cancel();
        });
        let started = std::time::Instant::now();
        assert!(!cancel.sleep(Duration::from_secs(30)).await);
        assert!(started.elapsed() < Duration::from_secs(5));
        stopper.await.unwrap();

        // Already cancelled: resolves at once, the work never runs.
        assert!(cancel.is_cancelled());
        assert_eq!(cancel.run(std::future::pending::<()>()).await, None);
        cancel.cancelled().await;
    }
}
