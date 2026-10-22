use super::{ComicService, HomeSectionMode, HomeSectionRequest};
use crate::{
    endpoint::{EndpointManager, EndpointProbe},
    expiring_cache::ExpiringCache,
    jm::{JmClient, JmError},
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU16, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

#[derive(Clone, Debug)]
struct Request {
    path: String,
    query: HashMap<String, String>,
    authorization: Option<String>,
}

struct SourceState {
    label: &'static str,
    total: usize,
    status: AtomicU16,
    delay_ms: AtomicU64,
    requests: Mutex<Vec<Request>>,
    blocked_auth: Mutex<Option<(String, Arc<Semaphore>)>>,
}

struct Source {
    endpoint: String,
    state: Arc<SourceState>,
    server: JoinHandle<()>,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Source {
    async fn new(label: &'static str, total: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(SourceState {
            label,
            total,
            status: AtomicU16::new(200),
            delay_ms: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
            blocked_auth: Mutex::new(None),
        });
        let app = Router::new()
            .fallback(source_response)
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint,
            state,
            server,
        }
    }

    async fn requests(&self) -> Vec<Request> {
        self.state.requests.lock().await.clone()
    }
}

async fn source_response(
    State(state): State<Arc<SourceState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let url = reqwest::Url::parse(&format!("http://localhost{uri}")).unwrap();
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let authorization = headers
        .get("authorization")
        .map(|value| value.to_str().unwrap().to_string());
    state.requests.lock().await.push(Request {
        path: uri.path().to_string(),
        query: query.clone(),
        authorization: authorization.clone(),
    });
    let gate = state
        .blocked_auth
        .lock()
        .await
        .as_ref()
        .filter(|(expected, _)| authorization.as_ref() == Some(expected))
        .map(|(_, gate)| gate.clone());
    if let Some(gate) = gate {
        gate.acquire().await.unwrap().forget();
    }
    tokio::time::sleep(Duration::from_millis(state.delay_ms.load(Ordering::SeqCst))).await;
    let status = state.status.load(Ordering::SeqCst);
    if status != 200 {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"code": status, "errorMsg": "test source unavailable"})),
        )
            .into_response();
    }
    let page = query.get("page").unwrap().parse::<usize>().unwrap();
    let (size, page) = if uri.path() == "/serialization" {
        (40, page.saturating_sub(1))
    } else {
        (80, page)
    };
    let start = size * page;
    let count = state.total.saturating_sub(start).min(size);
    let items = (start..start + count).map(|index| json!({
        "id": (index + 1).to_string(),
        "name": format!("{}:{}:{}", state.label, authorization.as_deref().unwrap_or("anonymous"), index + 1),
        "author": "Test author",
        "image": "https://image.example/cover.jpg"
    })).collect::<Vec<_>>();
    let data = match uri.path() {
        "/latest" => json!(items),
        "/serialization" => json!({"list": items}),
        "/categories/filter" => json!({"total": state.total, "content": items}),
        _ => panic!("unexpected test path: {}", uri.path()),
    };
    Json(json!({"code": 200, "data": data})).into_response()
}

async fn service(sources: &[&Source]) -> ComicService {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&db).await.unwrap();
    let endpoints = Arc::new(EndpointManager::new(db).await.unwrap());
    endpoints
        .set_probes_for_test(
            sources
                .iter()
                .enumerate()
                .map(|(index, source)| EndpointProbe {
                    endpoint: source.endpoint.clone(),
                    available: true,
                    latency_ms: Some(index as u64 + 1),
                    error: None,
                })
                .collect(),
        )
        .await;
    let mut jm = JmClient::new().unwrap();
    jm.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    ComicService::new(Arc::new(jm), endpoints)
}

fn request(mode: HomeSectionMode, page: u32) -> HomeSectionRequest {
    HomeSectionRequest {
        mode,
        page,
        section_id: String::new(),
        section_title: "Test section".into(),
        filter_value: String::new(),
        category: "all".into(),
        week: "2".into(),
        order: "new".into(),
    }
}

#[tokio::test]
async fn ranking_and_latest_reuse_eighty_item_source_pages() {
    for mode in [HomeSectionMode::Ranking, HomeSectionMode::Latest] {
        let source = Source::new("source", 93).await;
        let service = service(&[&source]).await;
        let cloned = service.clone();
        for page in 1..=5 {
            let result = cloned
                .get_home_section_list(request(mode, page))
                .await
                .unwrap();
            let start = (page - 1) * 20;
            let count = (93 - start).min(20);
            assert_eq!(result.page, page);
            assert_eq!(result.page_size, 20);
            assert_eq!(result.title, "Test section");
            assert_eq!(result.items.len(), count as usize);
            assert_eq!(result.items[0].id, (start + 1).to_string());
            assert_eq!(result.items.last().unwrap().id, (start + count).to_string());
            assert_eq!(result.has_more, page < 5);
            assert_eq!(
                result.total,
                if matches!(mode, HomeSectionMode::Ranking) {
                    93
                } else {
                    0
                }
            );
        }
        let mut renamed = request(mode, 2);
        renamed.section_title = "Another title".into();
        assert_eq!(
            service.get_home_section_list(renamed).await.unwrap().title,
            "Another title"
        );
        let calls = source.requests().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].query["page"], "0");
        assert_eq!(calls[1].query["page"], "1");
    }
}

#[tokio::test]
async fn weekly_updates_reuse_forty_item_source_pages() {
    let source = Source::new("weekly", 53).await;
    let service = service(&[&source]).await;
    for page in 1..=3 {
        let result = service
            .get_home_section_list(request(HomeSectionMode::Weekly, page))
            .await
            .unwrap();
        assert_eq!(result.page_size, 20);
        assert_eq!(result.items[0].id, ((page - 1) * 20 + 1).to_string());
        assert_eq!(result.items.len(), if page == 3 { 13 } else { 20 });
        assert_eq!(result.has_more, page < 3);
    }
    let calls = source.requests().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].query["page"], "1");
    assert_eq!(calls[1].query["page"], "2");
    assert_eq!(calls[0].query["date"], "2");
    assert_eq!(calls[0].query["type"], "all");
}

#[tokio::test]
async fn concurrent_subpages_share_a_single_source_request() {
    let source = Source::new("concurrent", 160).await;
    source.state.delay_ms.store(30, Ordering::SeqCst);
    let service = service(&[&source]).await;
    let mut jobs = JoinSet::new();
    for index in 0..8 {
        let service = service.clone();
        jobs.spawn(async move {
            service
                .get_home_section_list(request(HomeSectionMode::Ranking, index % 4 + 1))
                .await
        });
    }
    while let Some(result) = jobs.join_next().await {
        assert_eq!(result.unwrap().unwrap().items.len(), 20);
    }
    assert_eq!(source.requests().await.len(), 1);
}

#[tokio::test]
async fn source_cache_separates_paths_categories_orders_and_weekdays() {
    let source = Source::new("filters", 160).await;
    let service = service(&[&source]).await;
    let cases = [
        (HomeSectionMode::Latest, "all", "new", "2"),
        (HomeSectionMode::Ranking, "all", "new", "2"),
        (HomeSectionMode::Ranking, "month", "new", "2"),
        (HomeSectionMode::Ranking, "month", "mv", "2"),
        (HomeSectionMode::Weekly, "all", "new", "2"),
        (HomeSectionMode::Weekly, "hanman", "new", "2"),
        (HomeSectionMode::Weekly, "hanman", "new", "3"),
    ];
    for (mode, category, order, week) in cases {
        for page in [1, 2] {
            let mut query = request(mode, page);
            query.category = category.into();
            query.order = order.into();
            query.week = week.into();
            service.get_home_section_list(query).await.unwrap();
        }
    }
    let calls = source.requests().await;
    assert_eq!(calls.len(), cases.len());
    assert_eq!(calls[0].path, "/latest");
    assert_eq!(calls[2].query["c"], "month");
    assert_eq!(calls[3].query["o"], "mv");
    assert_eq!(calls[6].query["date"], "3");
}

#[tokio::test]
async fn source_cache_is_partitioned_across_login_and_logout() {
    let source = Source::new("account", 160).await;
    let service = service(&[&source]).await;
    for token in [
        Some("first-user"),
        Some("second-user"),
        None,
        Some("first-user"),
    ] {
        service.jm.set_jwt_token(token.map(str::to_string)).await;
        for page in [1, 2] {
            let result = service
                .get_home_section_list(request(HomeSectionMode::Ranking, page))
                .await
                .unwrap();
            let auth = token
                .map(|token| format!("Bearer {token}"))
                .unwrap_or_else(|| "anonymous".into());
            assert!(result.items[0].title.contains(&auth));
        }
    }
    let calls = source.requests().await;
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].authorization.as_deref(), Some("Bearer first-user"));
    assert_eq!(
        calls[1].authorization.as_deref(),
        Some("Bearer second-user")
    );
    assert_eq!(calls[2].authorization, None);
}

#[tokio::test]
async fn endpoint_changes_isolate_cache_and_cache_hits_preserve_probe_latency() {
    let first = Source::new("first", 160).await;
    let second = Source::new("second", 160).await;
    let service = service(&[&first, &second]).await;
    let result = service
        .get_home_section_list(request(HomeSectionMode::Ranking, 1))
        .await
        .unwrap();
    assert!(result.items[0].title.starts_with("first:"));
    service.endpoints.report_success(&second.endpoint, 45).await;
    let result = service
        .get_home_section_list(request(HomeSectionMode::Ranking, 2))
        .await
        .unwrap();
    assert!(result.items[0].title.starts_with("second:"));
    service.endpoints.report_success(&first.endpoint, 123).await;
    let result = service
        .get_home_section_list(request(HomeSectionMode::Ranking, 3))
        .await
        .unwrap();
    assert!(result.items[0].title.starts_with("first:"));
    assert_eq!(first.requests().await.len(), 1);
    assert_eq!(second.requests().await.len(), 1);
    let state = service.endpoints.state().await;
    assert_eq!(
        state
            .endpoints
            .iter()
            .find(|probe| probe.endpoint == first.endpoint)
            .unwrap()
            .latency_ms,
        Some(123)
    );
}

#[tokio::test]
async fn failed_source_responses_are_not_cached() {
    let source = Source::new("errors", 160).await;
    let service = service(&[&source]).await;
    source.state.status.store(400, Ordering::SeqCst);
    assert!(matches!(
        service
            .get_home_section_list(request(HomeSectionMode::Ranking, 1))
            .await,
        Err(JmError::Api(_))
    ));
    source.state.status.store(200, Ordering::SeqCst);
    service
        .get_home_section_list(request(HomeSectionMode::Ranking, 1))
        .await
        .unwrap();
    service
        .get_home_section_list(request(HomeSectionMode::Ranking, 2))
        .await
        .unwrap();
    assert_eq!(source.requests().await.len(), 2);
}

#[tokio::test]
async fn cached_fallback_pages_keep_the_actual_source_endpoint() {
    let first = Source::new("unavailable", 160).await;
    let second = Source::new("fallback", 160).await;
    first.state.status.store(503, Ordering::SeqCst);
    first.state.delay_ms.store(30, Ordering::SeqCst);
    let service = service(&[&first, &second]).await;
    let mut jobs = JoinSet::new();
    for page in 1..=4 {
        let service = service.clone();
        jobs.spawn(async move {
            service
                .get_home_section_list(request(HomeSectionMode::Ranking, page))
                .await
        });
    }
    while let Some(result) = jobs.join_next().await {
        let result = result.unwrap().unwrap();
        assert!(result.items[0].title.starts_with("fallback:"));
    }
    assert_eq!(first.requests().await.len(), 1);
    assert_eq!(second.requests().await.len(), 1);
    first.state.status.store(200, Ordering::SeqCst);
    service.endpoints.report_success(&first.endpoint, 10).await;
    let result = service
        .get_home_section_list(request(HomeSectionMode::Ranking, 3))
        .await
        .unwrap();
    assert!(result.items[0].title.starts_with("unavailable:"));
    assert_eq!(first.requests().await.len(), 2);
}

#[tokio::test]
async fn expired_source_pages_are_requested_again() {
    let source = Source::new("expired", 160).await;
    let mut service = service(&[&source]).await;
    service.source_pages = Arc::new(ExpiringCache::new(Duration::ZERO, 128));
    for page in [1, 2] {
        service
            .get_home_section_list(request(HomeSectionMode::Ranking, page))
            .await
            .unwrap();
    }
    assert_eq!(source.requests().await.len(), 2);
}

#[tokio::test]
async fn source_page_cache_evicts_entries_at_its_capacity() {
    let source = Source::new("capacity", 1000).await;
    let mut service = service(&[&source]).await;
    service.source_pages = Arc::new(ExpiringCache::new(Duration::from_secs(300), 2));
    for page in [1, 5, 9, 6, 1] {
        service
            .get_home_section_list(request(HomeSectionMode::Ranking, page))
            .await
            .unwrap();
    }
    let calls = source.requests().await;
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls
            .iter()
            .map(|call| call.query["page"].as_str())
            .collect::<Vec<_>>(),
        ["0", "1", "2", "0"]
    );
}

#[tokio::test]
async fn empty_successful_source_pages_can_be_reused() {
    let source = Source::new("empty", 0).await;
    let service = service(&[&source]).await;
    for page in [1, 2] {
        let result = service
            .get_home_section_list(request(HomeSectionMode::Ranking, page))
            .await
            .unwrap();
        assert!(result.items.is_empty());
        assert_eq!(result.total, 0);
        assert!(!result.has_more);
    }
    assert_eq!(source.requests().await.len(), 1);
}

#[tokio::test]
async fn account_changes_during_failover_cannot_mix_cached_responses() {
    let first = Source::new("unavailable", 160).await;
    let fallback = Source::new("fallback", 160).await;
    first.state.status.store(503, Ordering::SeqCst);
    let gate = Arc::new(Semaphore::new(0));
    *first.state.blocked_auth.lock().await = Some(("Bearer first-user".into(), gate.clone()));
    let service = service(&[&first, &fallback]).await;
    service.jm.set_jwt_token(Some("first-user".into())).await;
    let old_request = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .get_home_section_list(request(HomeSectionMode::Ranking, 1))
                .await
        }
    });
    timeout(Duration::from_secs(1), async {
        while first.requests().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first account request should reach the upstream");

    service.jm.set_jwt_token(Some("second-user".into())).await;
    let new_result = timeout(
        Duration::from_secs(1),
        service.get_home_section_list(request(HomeSectionMode::Ranking, 2)),
    )
    .await
    .expect("new account should not wait for the old account request")
    .unwrap();
    assert!(new_result.items[0].title.contains("Bearer second-user"));
    gate.add_permits(1);
    let old_result = old_request.await.unwrap().unwrap();
    assert!(old_result.items[0].title.contains("Bearer first-user"));
    let cached = service
        .get_home_section_list(request(HomeSectionMode::Ranking, 3))
        .await
        .unwrap();
    assert!(cached.items[0].title.contains("Bearer second-user"));
    let calls = fallback.requests().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer second-user")
    );
    assert_eq!(calls[1].authorization.as_deref(), Some("Bearer first-user"));
}
