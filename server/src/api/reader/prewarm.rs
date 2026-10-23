use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PREWARM_CONCURRENCY: usize = 2;
const PREWARM_QUEUE_CAPACITY: usize = 32;
const PREWARM_MAX_QUEUE_AGE: Duration = Duration::from_secs(30);

type PageKey = (String, u32);

struct PendingPage {
    key: PageKey,
    requested_at: Instant,
    work: Pin<Box<dyn Future<Output = ()> + Send>>,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<PendingPage>,
    running: HashSet<PageKey>,
}

#[derive(Default)]
pub(super) struct PagePrewarmQueue {
    state: Mutex<QueueState>,
}

struct RunningPage {
    queue: Arc<PagePrewarmQueue>,
    key: PageKey,
}

impl PagePrewarmQueue {
    pub(super) fn enqueue(
        self: &Arc<Self>,
        chapter_id: String,
        page: u32,
        work: impl Future<Output = ()> + Send + 'static,
    ) {
        let key = (chapter_id, page);
        let ready = {
            let mut state = self.state.lock().expect("prewarm queue poisoned");
            let now = Instant::now();
            state.discard_expired(now);
            if state.running.contains(&key) {
                return;
            }

            // A fresh request renews this page without clearing another reader's chapter.
            if let Some(index) = state.pending.iter().position(|job| job.key == key) {
                state.pending.remove(index);
            }
            if state.pending.len() == PREWARM_QUEUE_CAPACITY {
                state.pending.pop_front();
            }
            state.pending.push_back(PendingPage {
                key,
                requested_at: now,
                work: Box::pin(work),
            });
            state.take_ready(now)
        };
        self.spawn_ready(ready);
    }

    fn spawn_ready(self: &Arc<Self>, jobs: Vec<PendingPage>) {
        for job in jobs {
            let running = RunningPage {
                queue: self.clone(),
                key: job.key,
            };
            tokio::spawn(async move {
                let _running = running;
                job.work.await;
            });
        }
    }
}

impl QueueState {
    fn discard_expired(&mut self, now: Instant) {
        self.pending
            .retain(|job| now.duration_since(job.requested_at) < PREWARM_MAX_QUEUE_AGE);
    }

    fn take_ready(&mut self, now: Instant) -> Vec<PendingPage> {
        self.discard_expired(now);
        let mut ready = Vec::new();
        while self.running.len() < PREWARM_CONCURRENCY {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            self.running.insert(job.key.clone());
            ready.push(job);
        }
        ready
    }
}

impl Drop for RunningPage {
    fn drop(&mut self) {
        let ready = {
            let mut state = self.queue.state.lock().expect("prewarm queue poisoned");
            state.running.remove(&self.key);
            state.take_ready(Instant::now())
        };
        self.queue.spawn_ready(ready);
    }
}

#[cfg(test)]
mod tests;
