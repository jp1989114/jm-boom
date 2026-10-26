use super::FALLBACK_ENDPOINTS;
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyInit};
use aes::Aes256;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ecb::Decryptor;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;
use tokio::task::JoinSet;

const HOST_CONFIG_AES_SEED: &str = "diosfjckwpqpdfjkvnqQjsik";
const HOST_CONFIG_URLS: [&str; 2] = [
    "https://rup4a04-c02.tos-cn-hongkong.bytepluses.com/newsvr-2025.txt",
    "https://rup4a04-c01.tos-ap-southeast-1.bytepluses.com/newsvr-2025.txt",
];

pub(super) async fn discover_candidates(client: &Client) -> Vec<String> {
    discover_candidates_from(client, &HOST_CONFIG_URLS).await
}

async fn discover_candidates_from(client: &Client, urls: &[&str]) -> Vec<String> {
    let mut candidates = FALLBACK_ENDPOINTS
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();

    let mut tasks = JoinSet::new();
    for url in urls {
        let client = client.clone();
        let url = (*url).to_string();
        tasks.spawn(async move {
            let result = fetch_host_config(&client, &url).await;
            (url, result)
        });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((_, Ok(hosts))) => {
                candidates.extend(hosts);
                break;
            }
            Ok((url, Err(error))) => tracing::warn!(url, %error, "failed to load endpoint config"),
            Err(error) => tracing::warn!(%error, "endpoint config task failed"),
        }
    }
    // Dropping the set cancels any source still waiting on the network.
    drop(tasks);

    let mut unique = HashSet::new();
    candidates
        .into_iter()
        .filter(|value| unique.insert(value.clone()))
        .collect()
}

async fn fetch_host_config(client: &Client, url: &str) -> anyhow::Result<Vec<String>> {
    let body = client
        .get(url)
        .header("accept", "text/plain,*/*")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    decode_host_config(&body)
}

fn decode_host_config(body: &str) -> anyhow::Result<Vec<String>> {
    let encoded = body
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
        .collect::<String>();
    let key = format!("{:x}", md5::compute(HOST_CONFIG_AES_SEED));
    let encrypted = BASE64.decode(encoded)?;
    let decryptor = Decryptor::<Aes256>::new_from_slice(key.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid endpoint config key length"))?;
    let decrypted = decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(&encrypted)
        .map_err(|error| anyhow::anyhow!("failed to decrypt endpoint config: {error}"))?;
    let payload: HostConfigPayload = serde_json::from_slice(&decrypted)?;
    let hosts: Vec<_> = payload
        .server
        .into_iter()
        .filter_map(|value| normalize_endpoint(&value).ok())
        .collect();
    if hosts.is_empty() {
        anyhow::bail!("host config did not include valid HTTPS endpoints");
    }
    Ok(hosts)
}

pub(super) fn normalize_endpoint(value: &str) -> anyhow::Result<String> {
    let value = value.trim().trim_end_matches('/');
    let value = if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let url = reqwest::Url::parse(&value)?;

    if url.scheme() != "https" || url.host_str().is_none() || url.path() != "/" {
        anyhow::bail!("Endpoint must be an HTTPS origin");
    }

    let host = url.host_str().expect("validated host");
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("https://{host}{port}"))
}

#[derive(Deserialize)]
struct HostConfigPayload {
    #[serde(default, rename = "Server")]
    server: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use axum::{routing::get, Router};
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::AsyncReadExt,
        sync::{oneshot, Notify},
        time::timeout,
    };

    fn encrypted_config(hosts: &[&str]) -> String {
        let key = format!("{:x}", md5::compute(HOST_CONFIG_AES_SEED));
        let body = serde_json::json!({ "Server": hosts }).to_string();
        BASE64.encode(
            ecb::Encryptor::<Aes256>::new_from_slice(key.as_bytes())
                .unwrap()
                .encrypt_padded_vec_mut::<Pkcs7>(body.as_bytes()),
        )
    }

    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/config", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn uses_the_first_valid_config_without_waiting_for_a_stalled_source() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slow_url = format!("http://{}/config", listener.local_addr().unwrap());
        let started = Arc::new(Notify::new());
        let (closed_tx, closed_rx) = oneshot::channel();
        let slow_task = tokio::spawn({
            let started = started.clone();
            async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                assert!(socket.read(&mut buffer).await.unwrap() > 0);
                started.notify_one();
                while socket.read(&mut buffer).await.unwrap() > 0 {}
                let _ = closed_tx.send(());
            }
        });
        let (fast_url, fast_task) = serve(Router::new().route(
            "/config",
            get(move || {
                let started = started.clone();
                async move {
                    started.notified().await;
                    encrypted_config(&[
                        "fast.example",
                        "https://fast.example/",
                        FALLBACK_ENDPOINTS[0],
                    ])
                }
            }),
        ))
        .await;
        let result = timeout(
            Duration::from_secs(2),
            discover_candidates_from(&client(), &[&slow_url, &fast_url]),
        )
        .await;
        fast_task.abort();
        let closed = timeout(Duration::from_secs(1), closed_rx).await;
        slow_task.abort();
        let hosts = result.expect("the valid source must not wait for the stalled source timeout");
        assert_eq!(
            hosts,
            [
                FALLBACK_ENDPOINTS[0],
                FALLBACK_ENDPOINTS[1],
                "https://fast.example"
            ]
        );
        closed
            .expect("the unused request must be cancelled")
            .unwrap();
    }

    #[tokio::test]
    async fn skips_configs_without_usable_endpoints_and_keeps_the_fallbacks() {
        for bad_body in [
            "not encrypted config".to_string(),
            encrypted_config(&[]),
            encrypted_config(&["", "http://insecure.example", "https://example.com/path"]),
        ] {
            let (bad_url, bad_task) = serve(Router::new().route(
                "/config",
                get(move || {
                    let body = bad_body.clone();
                    async move { body }
                }),
            ))
            .await;
            let (good_url, good_task) = serve(Router::new().route(
                "/config",
                get(|| async {
                    encrypted_config(&[
                        "good.example",
                        "https://good.example/",
                        "http://insecure.example",
                    ])
                }),
            ))
            .await;
            let hosts = discover_candidates_from(&client(), &[&bad_url, &good_url]).await;
            bad_task.abort();
            good_task.abort();
            assert_eq!(
                hosts,
                [
                    FALLBACK_ENDPOINTS[0],
                    FALLBACK_ENDPOINTS[1],
                    "https://good.example"
                ]
            );
        }
    }

    #[tokio::test]
    async fn failed_sources_leave_only_the_builtin_fallbacks() {
        let (bad_url, bad_task) = serve(Router::new().route(
            "/config",
            get(|| async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "unavailable") }),
        ))
        .await;
        let (invalid_url, invalid_task) =
            serve(Router::new().route("/config", get(|| async { "invalid ciphertext" }))).await;
        let hosts = discover_candidates_from(&client(), &[&bad_url, &invalid_url]).await;
        bad_task.abort();
        invalid_task.abort();
        assert_eq!(hosts, FALLBACK_ENDPOINTS);
    }
}
