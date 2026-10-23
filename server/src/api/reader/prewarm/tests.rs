use super::{
    PageKey, PagePrewarmQueue, PREWARM_CONCURRENCY, PREWARM_MAX_QUEUE_AGE, PREWARM_QUEUE_CAPACITY,
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{oneshot, Barrier, Semaphore},
    task::JoinSet,
};

#[derive(Default)]
struct WorkLog {
    started: Mutex<Vec<PageKey>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
}

fn enqueue(
    queue: &Arc<PagePrewarmQueue>,
    log: &Arc<WorkLog>,
    gate: &Arc<Semaphore>,
    chapter: &str,
    page: u32,
) {
    let key = (chapter.to_string(), page);
    let gate = gate.clone();
    let log = log.clone();
    queue.enqueue(chapter.to_string(), page, async move {
        log.started.lock().unwrap().push(key);
        let active = log.active.fetch_add(1, Ordering::SeqCst) + 1;
        log.peak.fetch_max(active, Ordering::SeqCst);
        gate.acquire().await.unwrap().forget();
        log.active.fetch_sub(1, Ordering::SeqCst);
        log.completed.fetch_add(1, Ordering::SeqCst);
    });
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prewarm work did not reach the expected state");
}

async fn wait_idle(queue: &PagePrewarmQueue) {
    wait_until(|| {
        let state = queue.state.lock().unwrap();
        state.pending.is_empty() && state.running.is_empty()
    })
    .await;
}

#[tokio::test]
async fn a_request_burst_retains_only_a_bounded_backlog_and_two_running_jobs() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    for page in 0..10_000 {
        enqueue(&queue, &log, &gate, "100", page);
    }
    {
        let state = queue.state.lock().unwrap();
        assert_eq!(state.running.len(), PREWARM_CONCURRENCY);
        assert_eq!(state.pending.len(), PREWARM_QUEUE_CAPACITY);
        assert_eq!(state.pending.front().unwrap().key.1, 9968);
        assert_eq!(state.pending.back().unwrap().key.1, 9999);
    }
    wait_until(|| log.active.load(Ordering::SeqCst) == PREWARM_CONCURRENCY).await;
    gate.add_permits(PREWARM_CONCURRENCY + PREWARM_QUEUE_CAPACITY);
    wait_idle(&queue).await;
    assert_eq!(log.peak.load(Ordering::SeqCst), PREWARM_CONCURRENCY);
    assert_eq!(log.completed.load(Ordering::SeqCst), 34);
    let started = log.started.lock().unwrap();
    assert!(started.iter().all(|(_, page)| *page < 2 || *page >= 9968));
}

#[tokio::test]
async fn overlapping_windows_deduplicate_running_and_pending_pages_across_readers() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    for _ in 0..100 {
        for page in 10..14 {
            enqueue(&queue, &log, &gate, "100", page);
        }
        enqueue(&queue, &log, &gate, "200", 12);
    }
    {
        let state = queue.state.lock().unwrap();
        assert_eq!(state.running.len(), 2);
        assert_eq!(state.pending.len(), 3);
    }
    gate.add_permits(5);
    wait_idle(&queue).await;
    let started = log.started.lock().unwrap();
    assert_eq!(started.len(), 5);
    assert_eq!(started.iter().collect::<HashSet<_>>().len(), 5);
    assert!(started.contains(&("100".into(), 12)));
    assert!(started.contains(&("200".into(), 12)));
}

#[tokio::test]
async fn expired_waiting_pages_are_dropped_before_they_can_run() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    for page in 0..4 {
        enqueue(&queue, &log, &gate, "100", page);
    }
    queue.state.lock().unwrap().pending[0].requested_at = Instant::now() - PREWARM_MAX_QUEUE_AGE;
    gate.add_permits(3);
    wait_idle(&queue).await;
    let started = log.started.lock().unwrap();
    assert_eq!(started.len(), 3);
    assert!(!started.contains(&("100".into(), 2)));
    assert!(started.contains(&("100".into(), 3)));
}

#[tokio::test]
async fn a_repeated_request_refreshes_its_age_without_dropping_another_chapter() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    for page in 0..3 {
        enqueue(&queue, &log, &gate, "100", page);
    }
    enqueue(&queue, &log, &gate, "200", 0);
    let renewed_at = Instant::now();
    queue.state.lock().unwrap().pending[0].requested_at = renewed_at - Duration::from_secs(20);
    enqueue(&queue, &log, &gate, "100", 2);
    {
        let state = queue.state.lock().unwrap();
        assert_eq!(state.pending.len(), 2);
        assert_eq!(state.pending.front().unwrap().key, ("200".into(), 0));
        assert_eq!(state.pending.back().unwrap().key, ("100".into(), 2));
        assert!(state.pending.back().unwrap().requested_at >= renewed_at);
    }
    gate.add_permits(4);
    wait_idle(&queue).await;
    assert_eq!(log.completed.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn enqueue_discards_expired_jobs_and_a_finished_page_can_be_requested_again() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    for page in 0..5 {
        enqueue(&queue, &log, &gate, "100", page);
    }
    {
        let mut state = queue.state.lock().unwrap();
        for job in &mut state.pending {
            job.requested_at = Instant::now() - PREWARM_MAX_QUEUE_AGE;
        }
    }
    enqueue(&queue, &log, &gate, "200", 0);
    assert_eq!(queue.state.lock().unwrap().pending.len(), 1);
    gate.add_permits(3);
    wait_idle(&queue).await;
    enqueue(&queue, &log, &gate, "200", 0);
    gate.add_permits(1);
    wait_idle(&queue).await;
    let started = log.started.lock().unwrap();
    assert_eq!(started.len(), 4);
    assert_eq!(
        started
            .iter()
            .filter(|key| **key == ("200".into(), 0))
            .count(),
        2
    );
}

#[tokio::test]
async fn failed_work_releases_its_slot_and_does_not_block_later_pages() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let (release, wait) = oneshot::channel::<()>();
    queue.enqueue("100".into(), 0, async move {
        assert!(wait.await.is_err());
    });
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    enqueue(&queue, &log, &gate, "100", 1);
    enqueue(&queue, &log, &gate, "100", 2);
    drop(release);
    wait_until(|| log.active.load(Ordering::SeqCst) == 2).await;
    gate.add_permits(2);
    wait_idle(&queue).await;
    assert_eq!(log.completed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn discarded_requests_release_their_captured_state_without_becoming_tasks() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let gate = Arc::new(Semaphore::new(0));
    let captured = Arc::new(());
    for page in 0..10_000 {
        let capture = captured.clone();
        let gate = gate.clone();
        queue.enqueue("100".into(), page, async move {
            gate.acquire().await.unwrap().forget();
            drop(capture);
        });
        assert!(Arc::strong_count(&captured) <= PREWARM_QUEUE_CAPACITY + PREWARM_CONCURRENCY + 1);
    }
    assert_eq!(
        Arc::strong_count(&captured),
        PREWARM_QUEUE_CAPACITY + PREWARM_CONCURRENCY + 1
    );
    gate.add_permits(PREWARM_QUEUE_CAPACITY + PREWARM_CONCURRENCY);
    wait_idle(&queue).await;
    assert_eq!(Arc::strong_count(&captured), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readers_share_slots_and_keep_each_chapters_pages_distinct() {
    let queue = Arc::new(PagePrewarmQueue::default());
    let log = Arc::new(WorkLog::default());
    let gate = Arc::new(Semaphore::new(0));
    let barrier = Arc::new(Barrier::new(8));
    let mut readers = JoinSet::new();
    for reader in 0..8 {
        let queue = queue.clone();
        let log = log.clone();
        let gate = gate.clone();
        let barrier = barrier.clone();
        readers.spawn(async move {
            barrier.wait().await;
            let chapter = (100 + reader % 4).to_string();
            for request in 0..100 {
                enqueue(&queue, &log, &gate, &chapter, request % 4);
                tokio::task::yield_now().await;
            }
        });
    }
    while let Some(result) = readers.join_next().await {
        result.unwrap();
    }
    {
        let state = queue.state.lock().unwrap();
        assert_eq!(state.running.len(), 2);
        assert_eq!(state.pending.len(), 14);
    }
    gate.add_permits(16);
    wait_idle(&queue).await;
    let started = log.started.lock().unwrap();
    assert_eq!(started.len(), 16);
    assert_eq!(started.iter().collect::<HashSet<_>>().len(), 16);
    assert_eq!(log.peak.load(Ordering::SeqCst), 2);
}
