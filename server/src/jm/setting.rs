use super::{crypto, error::JmError, JmResult, SettingRequestSignature, API_SECRET};
use crate::{expiring_cache::ExpiringCache, keyed_lock::KeyedLock};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::time::Duration;

const IMG_HOST_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const IMG_HOST_CACHE_MAX_ENTRIES: usize = 32;
const MAX_IMAGE_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;
const SETTING_AES_SEEDS: [&str; 2] = [API_SECRET, "18comicAPPContent"];

static IMG_HOST_CACHE: Lazy<ExpiringCache<String>> =
    Lazy::new(|| ExpiringCache::new(IMG_HOST_CACHE_TTL, IMG_HOST_CACHE_MAX_ENTRIES));
static IMG_HOST_FETCH_LOCKS: Lazy<KeyedLock> = Lazy::new(KeyedLock::new);

#[derive(Debug, Deserialize)]
struct SettingResponse {
    #[serde(default)]
    code: i32,
    data: Option<serde_json::Value>,
    #[serde(default, rename = "errorMsg")]
    error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SettingData {
    #[serde(default, deserialize_with = "string_from_scalar")]
    img_host: String,
}

impl super::client::JmClient {
    pub async fn get_img_host(&self, endpoint: &str) -> JmResult<String> {
        if let Some(host) = cached_img_host(endpoint).await {
            return Ok(host);
        }

        let _guard = IMG_HOST_FETCH_LOCKS.lock(endpoint).await;
        if let Some(host) = cached_img_host(endpoint).await {
            return Ok(host);
        }

        fetch_and_cache_img_host(&self.client, endpoint).await
    }

    pub async fn download_image(&self, url: &str) -> JmResult<Vec<u8>> {
        download_image_bytes(&self.client, url, MAX_IMAGE_DOWNLOAD_BYTES).await
    }
}

pub(crate) async fn download_image_bytes(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> JmResult<Vec<u8>> {
    let mut response = client
        .get(url)
        .header("referer", "https://18comic.vip/")
        .send()
        .await
        .map_err(|error| JmError::Network(error.to_string()))?;

    if !response.status().is_success() {
        return Err(JmError::ImageHttp {
            status: response.status(),
            url: url.to_string(),
        });
    }

    let content_length = response.content_length();
    if let Some(content_length) = content_length {
        ensure_image_size(content_length, limit)?;
    }
    let capacity = content_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default();
    let mut data = Vec::with_capacity(capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| JmError::Network(error.to_string()))?
    {
        append_image_chunk(&mut data, &chunk, limit)?;
    }
    Ok(data)
}

// Probes must reach the server even when a usable image host is already cached.
pub(crate) async fn refresh_img_host(client: &reqwest::Client, endpoint: &str) -> JmResult<String> {
    let _guard = IMG_HOST_FETCH_LOCKS.lock(endpoint).await;
    fetch_and_cache_img_host(client, endpoint).await
}

async fn fetch_and_cache_img_host(client: &reqwest::Client, endpoint: &str) -> JmResult<String> {
    let signature = SettingRequestSignature::current();
    let url = format!("{endpoint}/setting");
    let response = client
        .get(&url)
        .header("accept", "application/json")
        .header("token", &signature.token)
        .header("tokenparam", &signature.tokenparam)
        .query(&[("app_img_shunt", "1"), ("t", signature.ts.as_str())])
        .send()
        .await
        .map_err(|error| JmError::Network(error.to_string()))?;

    if !response.status().is_success() {
        return Err(JmError::Http(format!("HTTP {}: {url}", response.status())));
    }

    let body = response
        .text()
        .await
        .map_err(|error| JmError::Network(error.to_string()))?;
    let envelope: SettingResponse = serde_json::from_str(&body)
        .map_err(|error| JmError::Decode(format!("Invalid setting response: {error}")))?;

    if envelope.code != 200 {
        return Err(JmError::Api(
            envelope
                .error_msg
                .unwrap_or_else(|| format!("API code {}", envelope.code)),
        ));
    }

    let value = envelope.data.ok_or(JmError::MissingData)?;
    let setting = match value {
        serde_json::Value::String(encrypted) => {
            let decrypted = decrypt_setting_data(&encrypted, &signature.ts)?;
            serde_json::from_str::<SettingData>(&decrypted)
                .map_err(|error| JmError::Decode(format!("Invalid setting data: {error}")))?
        }
        value => serde_json::from_value::<SettingData>(value)
            .map_err(|error| JmError::Decode(format!("Invalid setting data: {error}")))?,
    };

    let host = normalize_img_host(&setting.img_host)?;
    insert_cached_img_host(endpoint, host.clone()).await;
    tracing::debug!(endpoint, img_host = %host, "cached image host");
    Ok(host)
}

fn normalize_img_host(value: &str) -> JmResult<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Err(JmError::MissingData);
    }
    let value = if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let url = reqwest::Url::parse(&value)
        .map_err(|error| JmError::Decode(format!("Invalid image host: {error}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(JmError::Decode(
            "Image host must be an HTTP(S) origin".into(),
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn append_image_chunk(data: &mut Vec<u8>, chunk: &[u8], limit: usize) -> JmResult<()> {
    let next_size = data
        .len()
        .checked_add(chunk.len())
        .map(|size| size as u64)
        .unwrap_or(u64::MAX);
    ensure_image_size(next_size, limit)?;
    data.extend_from_slice(chunk);
    Ok(())
}

fn ensure_image_size(actual_bytes: u64, limit: usize) -> JmResult<()> {
    if actual_bytes > limit as u64 {
        return Err(JmError::ImageTooLarge {
            actual_bytes,
            limit_bytes: limit as u64,
        });
    }

    Ok(())
}

fn decrypt_setting_data(data: &str, ts: &str) -> JmResult<String> {
    let mut last_error = None;

    for seed in SETTING_AES_SEEDS {
        let key = format!("{:x}", md5::compute(format!("{ts}{seed}")));
        match crypto::decrypt_base64(data, &key) {
            Ok(decrypted) => return Ok(decrypted),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| JmError::Decrypt("Unable to decrypt setting data".into())))
}

fn string_from_scalar<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(value) => value,
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        _ => String::new(),
    })
}

pub async fn invalidate_img_host(endpoint: &str) {
    IMG_HOST_CACHE.remove(endpoint).await;
}

async fn cached_img_host(endpoint: &str) -> Option<String> {
    IMG_HOST_CACHE.get(endpoint).await
}

async fn insert_cached_img_host(endpoint: &str, host: String) {
    IMG_HOST_CACHE.insert(endpoint, host).await;
}

#[cfg(test)]
mod tests {
    use super::{append_image_chunk, ensure_image_size, normalize_img_host};
    use crate::jm::JmError;

    #[test]
    fn normalizes_image_hosts_without_accepting_paths_or_credentials() {
        for (input, expected) in [
            (" images.example/ ", "https://images.example"),
            ("https://IMAGES.example/", "https://images.example"),
            ("http://127.0.0.1:1234", "http://127.0.0.1:1234"),
        ] {
            assert_eq!(normalize_img_host(input).unwrap(), expected);
        }
        for input in [
            "",
            " ",
            "ftp://images.example",
            "https://images.example/path",
            "https://images.example?x=1",
            "https://images.example/#x",
            "https://user:pass@images.example",
            "bad host",
        ] {
            assert!(normalize_img_host(input).is_err(), "{input}");
        }
    }

    #[test]
    fn enforces_declared_and_streamed_image_size_limits() {
        assert!(ensure_image_size(8, 8).is_ok());
        assert!(matches!(
            ensure_image_size(9, 8),
            Err(JmError::ImageTooLarge {
                actual_bytes: 9,
                limit_bytes: 8
            })
        ));

        let mut data = vec![1, 2, 3, 4];
        append_image_chunk(&mut data, &[5, 6, 7, 8], 8).expect("append within limit");
        assert_eq!(data.len(), 8);
        let error = append_image_chunk(&mut data, &[9], 8).expect_err("reject oversized chunk");
        assert!(matches!(error, JmError::ImageTooLarge { .. }));
        assert!(!error.is_retryable());
        assert_eq!(data.len(), 8);
    }
}
