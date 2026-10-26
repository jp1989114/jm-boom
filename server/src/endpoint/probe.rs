use super::EndpointProbe;
use crate::jm::refresh_img_host;
use reqwest::Client;
use std::time::Instant;
use tokio::task::JoinSet;

const API_PROBE_CONCURRENCY: usize = 6;

pub(super) async fn probe_all(
    client: &Client,
    candidates: Vec<String>,
) -> Vec<(EndpointProbe, Option<String>)> {
    let mut candidates = candidates.into_iter();
    let mut tasks = JoinSet::new();
    let mut probes = Vec::new();
    loop {
        while tasks.len() < API_PROBE_CONCURRENCY {
            let Some(endpoint) = candidates.next() else {
                break;
            };
            let client = client.clone();
            tasks.spawn(async move { probe_endpoint(&client, endpoint).await });
        }
        match tasks.join_next().await {
            Some(Ok(probe)) => probes.push(probe),
            Some(Err(error)) => tracing::warn!(%error, "endpoint probe task failed"),
            None => break,
        }
    }
    probes
}

pub(super) async fn probe_endpoint(
    client: &Client,
    endpoint: String,
) -> (EndpointProbe, Option<String>) {
    let started = Instant::now();
    let result = refresh_img_host(client, &endpoint).await;

    match result {
        Ok(host) => (
            EndpointProbe {
                endpoint,
                available: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: None,
            },
            Some(host),
        ),
        Err(error) => (
            EndpointProbe {
                endpoint,
                available: false,
                latency_ms: None,
                error: Some(error.to_string()),
            },
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{probe_all, probe_endpoint, API_PROBE_CONCURRENCY};
    use crate::jm::{JmClient, API_SECRET, API_VERSION};
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit};
    use axum::{extract::Query, http::HeaderMap, routing::get, Router};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[tokio::test]
    async fn rejects_invalid_setting_responses_despite_http_success() {
        for body in [
            "<html>Access denied</html>",
            "",
            "not JSON",
            r#"{"code":403,"errorMsg":"denied","data":{"img_host":"https://images.example"}}"#,
            r#"{"code":200}"#,
            r#"{"code":200,"data":{}}"#,
            r#"{"code":200,"data":{"img_host":" "}}"#,
            r#"{"code":200,"data":"not encrypted data"}"#,
        ] {
            let (endpoint, server) =
                serve(Router::new().route("/setting", get(move || async move { body }))).await;
            let (probe, _) = probe_endpoint(&client(), endpoint.clone()).await;
            server.abort();
            assert!(!probe.available, "accepted {body}");
            assert!(probe.latency_ms.is_none());
            assert!(probe.error.is_some());
            assert_eq!(probe.endpoint, endpoint);
        }
    }

    #[tokio::test]
    async fn probes_always_request_settings_and_refresh_the_reader_host_cache() {
        let requests = Arc::new(AtomicUsize::new(0));
        let (endpoint, server) = serve(Router::new().route(
            "/setting",
            get({
                let requests = requests.clone();
                move || {
                    let count = requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if count == 2 {
                            return axum::Json(serde_json::json!({ "code": 200, "data": {} }));
                        }
                        axum::Json(serde_json::json!({
                            "code": 200,
                            "data": { "img_host": format!("https://images-{count}.example/") }
                        }))
                    }
                }
            }),
        ))
        .await;
        let http = client();
        let mut jm = JmClient::new().unwrap();
        jm.client = http.clone();
        for count in 0..2 {
            let (probe, _) = probe_endpoint(&http, endpoint.clone()).await;
            assert!(probe.available, "{probe:?}");
            assert!(probe.latency_ms.is_some());
            assert!(probe.error.is_none());
            assert_eq!(
                jm.get_img_host(&endpoint).await.unwrap(),
                format!("https://images-{count}.example")
            );
            assert_eq!(requests.load(Ordering::SeqCst), count + 1);
        }
        let (failed, _) = probe_endpoint(&http, endpoint.clone()).await;
        assert!(!failed.available);
        assert_eq!(
            jm.get_img_host(&endpoint).await.unwrap(),
            "https://images-1.example"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn validates_signatures_and_decrypts_both_setting_key_variants() {
        for seed in [API_SECRET, "18comicAPPContent"] {
            let (endpoint, server) = serve(Router::new().route("/setting", get(
                move |Query(query): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                    let ts = &query["t"];
                    assert_eq!(query["app_img_shunt"], "1");
                    assert_eq!(headers["tokenparam"], format!("{ts},{API_VERSION}"));
                    assert_eq!(headers["token"], format!("{:x}", md5::compute(format!("{ts}{API_SECRET}"))));
                    let key = format!("{:x}", md5::compute(format!("{ts}{seed}")));
                    let encrypted = ecb::Encryptor::<aes::Aes256>::new_from_slice(key.as_bytes())
                        .unwrap()
                        .encrypt_padded_vec_mut::<Pkcs7>(br#"{"img_host":"https://encrypted.example"}"#);
                    axum::Json(serde_json::json!({ "code": 200, "data": BASE64.encode(encrypted) }))
                }
            ))).await;
            let (probe, _) = probe_endpoint(&client(), endpoint.clone()).await;
            server.abort();
            assert!(probe.available, "{seed}: {probe:?}");
            assert_eq!(
                JmClient::new()
                    .unwrap()
                    .get_img_host(&endpoint)
                    .await
                    .unwrap(),
                "https://encrypted.example"
            );
        }
    }

    #[tokio::test]
    async fn rejects_http_errors_and_respects_the_probe_client_timeout() {
        let (endpoint, server) = serve(Router::new().route(
            "/setting",
            get(|| async {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    r#"{"code":200,"data":{"img_host":"https://images.example"}}"#,
                )
            }),
        ))
        .await;
        let (probe, _) = probe_endpoint(&client(), endpoint).await;
        server.abort();
        assert!(!probe.available);
        assert!(probe.error.unwrap().contains("503"));

        let (endpoint, server) = serve(Router::new().route(
            "/setting",
            get(|| async { std::future::pending::<String>().await }),
        ))
        .await;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let (probe, _) =
            tokio::time::timeout(Duration::from_secs(2), probe_endpoint(&http, endpoint))
                .await
                .unwrap();
        server.abort();
        assert!(!probe.available);
        assert!(probe.latency_ms.is_none());
        assert!(probe.error.is_some());
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn probes_every_candidate_with_a_bounded_number_of_active_requests() {
        let count = API_PROBE_CONCURRENCY + 3;
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (endpoint, server) = serve(Router::new().fallback({
            let started = started.clone();
            let release = release.clone();
            let active = active.clone();
            let peak = peak.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                let active = active.clone();
                let peak = peak.clone();
                async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    started.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    active.fetch_sub(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({ "code": 200, "data": { "img_host": "https://images.example" } }))
                }
            }
        })).await;
        let candidates: Vec<_> = (0..count)
            .map(|index| format!("{endpoint}/{index}"))
            .collect();
        let expected = candidates.clone();
        let task = tokio::spawn(async move { probe_all(&client(), candidates).await });
        tokio::time::timeout(
            Duration::from_secs(2),
            started.acquire_many(API_PROBE_CONCURRENCY as u32),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
        let exceeded = tokio::time::timeout(Duration::from_millis(100), started.acquire())
            .await
            .is_ok();
        release.add_permits(count);
        let probes = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert!(
            !exceeded,
            "a seventh request started before a slot was released"
        );
        assert!(peak.load(Ordering::SeqCst) <= API_PROBE_CONCURRENCY);
        assert_eq!(probes.len(), count);
        let mut actual = probes
            .into_iter()
            .map(|(probe, host)| {
                assert!(probe.available, "{probe:?}");
                assert_eq!(host.as_deref(), Some("https://images.example"));
                probe.endpoint
            })
            .collect::<Vec<_>>();
        actual.sort();
        assert_eq!(actual, expected);
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, server)
    }
}
