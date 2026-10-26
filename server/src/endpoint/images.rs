use super::{request_with_failover, EndpointManager};
use crate::{
    jm::{download_image_bytes, invalidate_img_host, JmClient, JmError, JmResult},
    reader::PageImageFormat,
};
use std::time::Instant;
use tokio::task::JoinSet;

const IMAGE_PROBE_PATH: &str = "/media/users/nopic-.gif?v=0";
const IMAGE_PROBE_MAX_BYTES: usize = 1024 * 1024;
const IMAGE_PROBE_CONCURRENCY: usize = 6;
const MAX_IMAGE_HOST_ATTEMPTS: usize = 4;

impl EndpointManager {
    pub(super) async fn refresh_image_hosts(&self, hosts: Vec<String>) {
        let mut hosts = hosts.into_iter();
        let mut tasks = JoinSet::new();
        let mut available = Vec::new();
        loop {
            while tasks.len() < IMAGE_PROBE_CONCURRENCY {
                let Some(host) = hosts.next() else { break };
                let client = self.client.clone();
                tasks.spawn(async move { probe_image_host(&client, host).await });
            }
            match tasks.join_next().await {
                Some(Ok(Some(probe))) => available.push(probe),
                Some(_) => {}
                None => break,
            }
        }
        available.sort_by_key(|(_, latency)| *latency);
        // A failed refresh must not discard the last usable pool.
        if !available.is_empty() {
            self.inner.write().await.image_hosts =
                available.into_iter().map(|(host, _)| host).collect();
        }
    }

    pub(crate) async fn download_image(&self, jm: &JmClient, path: &str) -> JmResult<Vec<u8>> {
        let mut hosts = self.inner.read().await.image_hosts.clone();
        let mut initial_endpoint = None;
        if hosts.is_empty() {
            let (endpoint, host) = request_with_failover(jm, self, |client, endpoint| {
                Box::pin(client.get_img_host(endpoint))
            })
            .await?;
            initial_endpoint = Some(endpoint);
            hosts.push(host);
        }

        let mut last_error = None;
        for host in hosts.into_iter().take(MAX_IMAGE_HOST_ATTEMPTS) {
            match jm.download_image(&format!("{host}{path}")).await {
                Ok(data) => return Ok(data),
                Err(error) if error.is_retryable() => {
                    tracing::warn!(image_host = %host, %error, "image request failed, trying next host");
                    self.inner
                        .write()
                        .await
                        .image_hosts
                        .retain(|candidate| *candidate != host);
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        // Keep the setting refresh retry while the initial background probes are pending.
        if let Some(endpoint) = initial_endpoint {
            invalidate_img_host(&endpoint).await;
            let (_, host) = request_with_failover(jm, self, |client, endpoint| {
                Box::pin(client.get_img_host(endpoint))
            })
            .await?;
            return jm.download_image(&format!("{host}{path}")).await;
        }
        Err(last_error.unwrap_or(JmError::Empty))
    }
}

async fn probe_image_host(client: &reqwest::Client, host: String) -> Option<(String, u128)> {
    let started = Instant::now();
    let url = format!("{host}{IMAGE_PROBE_PATH}");
    let data = match download_image_bytes(client, &url, IMAGE_PROBE_MAX_BYTES).await {
        Ok(data) => data,
        Err(error) => {
            tracing::debug!(image_host = %host, %error, "image host probe failed");
            return None;
        }
    };
    if !PageImageFormat::detect(&data).is_ok_and(|format| format.is_complete(&data)) {
        tracing::debug!(image_host = %host, "image host probe returned an invalid image");
        return None;
    }
    Some((host, started.elapsed().as_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{tests::test_manager, EndpointMode};
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
        Router,
    };
    use std::{
        io::Cursor,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    fn image() -> Vec<u8> {
        let mut data = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        data.into_inner()
    }

    fn jm() -> JmClient {
        let mut jm = JmClient::new().unwrap();
        jm.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        jm
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (host, server)
    }

    async fn image_server(
        status: StatusCode,
        body: Vec<u8>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let (host, server) = serve(Router::new().fallback({
            let requests = requests.clone();
            move |uri: Uri| {
                assert!(uri.path().starts_with("/media/"));
                requests.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                async move { (status, body) }
            }
        }))
        .await;
        (host, requests, server)
    }

    #[tokio::test]
    async fn probes_require_complete_images_and_keep_the_pool_on_total_failure() {
        let mut manager = test_manager().await;
        manager.client = jm().client;
        let (good, good_requests, good_server) = image_server(StatusCode::OK, image()).await;
        let mut truncated = image();
        truncated.truncate(truncated.len() - 4);
        for body in [
            b"<html>blocked</html>".to_vec(),
            Vec::new(),
            truncated,
            vec![0; IMAGE_PROBE_MAX_BYTES + 1],
        ] {
            let (bad, _, bad_server) = image_server(StatusCode::OK, body).await;
            manager
                .refresh_image_hosts(vec![bad.clone(), good.clone()])
                .await;
            assert_eq!(manager.inner.read().await.image_hosts, vec![good.clone()]);
            manager.refresh_image_hosts(vec![bad]).await;
            assert_eq!(manager.inner.read().await.image_hosts, vec![good.clone()]);
            bad_server.abort();
        }
        assert_eq!(good_requests.load(Ordering::SeqCst), 4);
        good_server.abort();
    }

    #[tokio::test]
    async fn image_failover_is_shared_by_paths_and_does_not_change_the_api_selection() {
        let manager = test_manager().await;
        let (bad, bad_requests, bad_server) =
            image_server(StatusCode::SERVICE_UNAVAILABLE, Vec::new()).await;
        let (good, good_requests, good_server) = image_server(StatusCode::OK, image()).await;
        {
            let mut inner = manager.inner.write().await;
            inner.mode = EndpointMode::Manual;
            inner.selected_endpoint = Some(inner.current_endpoint.clone());
            inner.image_hosts = vec![bad, good.clone()];
        }
        let before = manager.state().await;
        let jm = jm();
        for path in ["/media/photos/123/001.png", "/media/albums/123_3x4.jpg"] {
            assert_eq!(manager.download_image(&jm, path).await.unwrap(), image());
        }
        assert_eq!(bad_requests.load(Ordering::SeqCst), 1);
        assert_eq!(good_requests.load(Ordering::SeqCst), 2);
        assert_eq!(manager.inner.read().await.image_hosts, vec![good]);
        let after = manager.state().await;
        assert_eq!(before.current_endpoint, after.current_endpoint);
        assert_eq!(before.selected_endpoint, after.selected_endpoint);
        assert_eq!(after.mode, EndpointMode::Manual);
        bad_server.abort();
        good_server.abort();
    }

    #[tokio::test]
    async fn permanent_http_errors_do_not_evict_hosts_or_try_another_host() {
        let manager = test_manager().await;
        let jm = jm();
        let (good, good_requests, good_server) = image_server(StatusCode::OK, image()).await;
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
        ] {
            let (host, requests, server) = image_server(status, Vec::new()).await;
            let hosts = vec![host, good.clone()];
            manager.inner.write().await.image_hosts = hosts.clone();
            let error = manager
                .download_image(&jm, "/media/photos/123/001.png")
                .await
                .unwrap_err();
            assert!(matches!(error, JmError::ImageHttp { status: actual, .. } if actual == status));
            assert!(!error.is_retryable());
            assert_eq!(manager.inner.read().await.image_hosts, hosts);
            assert_eq!(requests.load(Ordering::SeqCst), 1);
            server.abort();
        }
        assert_eq!(good_requests.load(Ordering::SeqCst), 0);
        good_server.abort();
    }

    #[tokio::test]
    async fn image_failover_stops_at_the_attempt_limit() {
        let manager = test_manager().await;
        let mut servers = Vec::new();
        let mut hosts = Vec::new();
        let mut counts = Vec::new();
        for _ in 0..MAX_IMAGE_HOST_ATTEMPTS + 1 {
            let (host, count, server) = image_server(StatusCode::BAD_GATEWAY, Vec::new()).await;
            hosts.push(host);
            counts.push(count);
            servers.push(server);
        }
        manager.inner.write().await.image_hosts = hosts.clone();
        assert!(manager
            .download_image(&jm(), "/media/photos/123/001.png")
            .await
            .is_err());
        for (index, count) in counts.iter().enumerate() {
            assert_eq!(
                count.load(Ordering::SeqCst),
                usize::from(index < MAX_IMAGE_HOST_ATTEMPTS)
            );
        }
        assert_eq!(
            manager.inner.read().await.image_hosts,
            hosts[MAX_IMAGE_HOST_ATTEMPTS..]
        );
        for server in servers {
            server.abort();
        }
    }

    #[tokio::test]
    async fn cancellation_does_not_start_failover_or_evict_the_host() {
        let manager = test_manager().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let (slow, slow_server) = serve(Router::new().fallback({
            let started = started.clone();
            move || {
                let started = started.clone();
                async move {
                    started.notify_one();
                    std::future::pending::<String>().await
                }
            }
        }))
        .await;
        let (good, good_requests, good_server) = image_server(StatusCode::OK, image()).await;
        let hosts = vec![slow, good];
        manager.inner.write().await.image_hosts = hosts.clone();
        let task = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .download_image(&jm(), "/media/photos/123/001.png")
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(manager.inner.read().await.image_hosts, hosts);
        assert_eq!(good_requests.load(Ordering::SeqCst), 0);
        slow_server.abort();
        good_server.abort();
    }

    #[tokio::test]
    async fn requests_before_probing_can_refresh_a_changed_image_host() {
        let manager = test_manager().await;
        let (bad, bad_requests, bad_server) =
            image_server(StatusCode::SERVICE_UNAVAILABLE, Vec::new()).await;
        let (good, good_requests, good_server) = image_server(StatusCode::OK, image()).await;
        let setting_requests = Arc::new(AtomicUsize::new(0));
        let (api, api_server) = serve(Router::new().route(
            "/setting",
            get({
                let setting_requests = setting_requests.clone();
                move || {
                    let host = if setting_requests.fetch_add(1, Ordering::SeqCst) == 0 {
                        bad.clone()
                    } else {
                        good.clone()
                    };
                    async move {
                        axum::Json(serde_json::json!({ "code": 200, "data": { "img_host": host } }))
                    }
                }
            }),
        ))
        .await;
        manager.inner.write().await.current_endpoint = api;
        assert_eq!(
            manager
                .download_image(&jm(), "/media/photos/123/001.png")
                .await
                .unwrap(),
            image()
        );
        assert_eq!(setting_requests.load(Ordering::SeqCst), 2);
        assert_eq!(bad_requests.load(Ordering::SeqCst), 1);
        assert_eq!(good_requests.load(Ordering::SeqCst), 1);
        api_server.abort();
        bad_server.abort();
        good_server.abort();
    }
}
