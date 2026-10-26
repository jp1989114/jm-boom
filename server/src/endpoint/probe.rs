use super::EndpointProbe;
use crate::jm::refresh_img_host;
use reqwest::Client;
use std::time::Instant;

pub(super) async fn probe_endpoint(client: &Client, endpoint: String) -> EndpointProbe {
    let started = Instant::now();
    let result = refresh_img_host(client, &endpoint).await;

    match result {
        Ok(_) => EndpointProbe {
            endpoint,
            available: true,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: None,
        },
        Err(error) => EndpointProbe {
            endpoint,
            available: false,
            latency_ms: None,
            error: Some(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::probe_endpoint;
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
            let probe = probe_endpoint(&client(), endpoint.clone()).await;
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
            let probe = probe_endpoint(&http, endpoint.clone()).await;
            assert!(probe.available, "{probe:?}");
            assert!(probe.latency_ms.is_some());
            assert!(probe.error.is_none());
            assert_eq!(
                jm.get_img_host(&endpoint).await.unwrap(),
                format!("https://images-{count}.example")
            );
            assert_eq!(requests.load(Ordering::SeqCst), count + 1);
        }
        let failed = probe_endpoint(&http, endpoint.clone()).await;
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
            let probe = probe_endpoint(&client(), endpoint.clone()).await;
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
        let probe = probe_endpoint(&client(), endpoint).await;
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
        let probe = tokio::time::timeout(Duration::from_secs(2), probe_endpoint(&http, endpoint))
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

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, server)
    }
}
