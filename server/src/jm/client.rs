use super::{
    crypto,
    error::JmError,
    models::{
        Comic, ComicDetailPayload, HomeSection, HomeSectionPayload, SearchPayload, SearchResult,
    },
    signature::JmRequestSignature,
    JmResult,
};
use crate::domain::comic::{ComicChapter, ComicDetail, RelatedComic};
use once_cell::sync::OnceCell;
use reqwest::{Client, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize};
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLock;

static HTTP_CLIENT: OnceCell<Client> = OnceCell::new();

const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 13; jm-boom Build/TQ1A.230305.002; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/120.0.6099.230 Mobile Safari/537.36";

/// JM API client
pub struct JmClient {
    pub(crate) client: Client,
    auth: Arc<RwLock<RequestAuth>>,
}

#[derive(Clone, Default)]
pub(crate) struct RequestAuth {
    pub(crate) generation: u64,
    token: Option<String>,
}

impl JmClient {
    /// Create a new JM client
    pub fn new() -> JmResult<Self> {
        Ok(Self {
            client: get_or_create_client()?,
            auth: Arc::new(RwLock::new(RequestAuth::default())),
        })
    }

    /// GET request to JM API
    pub async fn get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> JmResult<T> {
        let auth = self.request_auth().await;
        self.get_with_auth(endpoint, path, query, &auth).await
    }

    pub(crate) async fn request_auth(&self) -> RequestAuth {
        self.auth.read().await.clone()
    }

    pub(crate) async fn get_with_auth<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        path: &str,
        query: &[(&str, String)],
        auth: &RequestAuth,
    ) -> JmResult<T> {
        let signature = JmRequestSignature::new();
        let url = format!("{endpoint}/{path}");

        let mut request = self
            .client
            .get(&url)
            .apply_request_signature(&url, &signature)?
            .query(query);
        if let Some(jwt) = auth.token.as_ref() {
            request = request.header("Authorization", format!("Bearer {jwt}"));
        }
        let response = request
            .send()
            .await
            .map_err(|e| JmError::Network(e.to_string()))?;

        decode_response(response, &url, &signature).await
    }

    pub async fn post_form<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        path: &str,
        fields: &[(String, String)],
        authenticated: bool,
    ) -> JmResult<T> {
        let signature = JmRequestSignature::new();
        let url = format!("{endpoint}/{path}");
        let mut request = self
            .client
            .post(&url)
            .apply_request_signature(&url, &signature)?
            .form(fields);
        if authenticated {
            if let Some(jwt) = self.auth.read().await.token.as_ref() {
                request = request.header("Authorization", format!("Bearer {jwt}"));
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| JmError::Network(error.to_string()))?;

        decode_response(response, &url, &signature).await
    }

    pub async fn set_jwt_token(&self, token: Option<String>) {
        let mut auth = self.auth.write().await;
        auth.token = token;
        auth.generation = auth.generation.wrapping_add(1);
    }

    /// Search comics
    pub async fn search(
        &self,
        endpoint: &str,
        keyword: &str,
        page: u32,
        order: &str,
    ) -> JmResult<SearchResult> {
        let payload: SearchPayload = self
            .get(
                endpoint,
                "search",
                &[
                    ("search_query", keyword.to_string()),
                    ("page", page.to_string()),
                    ("o", order.to_string()),
                ],
            )
            .await?;

        Ok(SearchResult {
            total: payload.total,
            content: payload.content.into_iter().map(Comic::from).collect(),
            redirect_aid: payload.redirect_aid,
        })
    }

    /// Get comic detail
    pub async fn get_comic_detail(&self, endpoint: &str, comic_id: &str) -> JmResult<ComicDetail> {
        let payload: ComicDetailPayload = self
            .get(endpoint, "album", &[("id", comic_id.to_string())])
            .await?;

        Ok(ComicDetail {
            id: payload.id,
            title: payload.name,
            description: payload.description,
            image: payload.image,
            authors: payload.author,
            tags: payload.tags,
            actors: payload.actors,
            works: payload.works,
            total_views: payload.total_views,
            likes: payload.likes,
            comment_count: payload.comment_total,
            related_comics: payload
                .related_list
                .into_iter()
                .map(|related| RelatedComic {
                    id: related.id,
                    title: related.name,
                    author: related.author,
                    image: related.image,
                })
                .collect(),
            chapters: payload
                .series
                .into_iter()
                .map(|chapter| ComicChapter {
                    id: chapter.id,
                    title: chapter.name,
                    sort: chapter.sort,
                })
                .collect(),
        })
    }

    /// Get home feed sections
    pub async fn get_home_feed(&self, endpoint: &str) -> JmResult<Vec<HomeSection>> {
        let sections: Vec<HomeSectionPayload> = self.get(endpoint, "promote", &[]).await?;

        Ok(sections
            .into_iter()
            .filter(|s| !is_unsupported_section(&s.title))
            .map(|s| HomeSection {
                id: s.id,
                title: s.title,
                slug: s.slug,
                section_type: s.section_type,
                filter_val: s.filter_val,
                content: s
                    .content
                    .into_iter()
                    .take(20) // limit preview items
                    .map(Comic::from)
                    .collect(),
            })
            .collect())
    }
}

impl Default for JmClient {
    fn default() -> Self {
        Self::new().expect("Failed to create JM client")
    }
}

// Helper to filter unsupported sections
fn is_unsupported_section(title: &str) -> bool {
    let title = title.trim();
    matches!(
        title,
        "禁漫小说"
            | "禁漫书库"
            | "禁漫書庫"
            | "禁漫小說"
            | "最新成人APP"
            | "大人気エロ同人"
            | "人気のエロ動画"
            | "オススメ動画サイト"
    )
}

#[cfg(test)]
mod tests {
    use super::{decode_response, is_unsupported_section, JmError, JmRequestSignature, JmResult};
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use serde_json::{json, Value};

    const TEST_URL: &str = "https://upstream.example/test";

    #[test]
    fn filters_unsupported_home_sections() {
        for title in ["禁漫小说", "禁漫书库", "禁漫書庫", "禁漫小說"] {
            assert!(is_unsupported_section(title));
        }

        assert!(!is_unsupported_section("每周推荐"));
    }

    #[tokio::test]
    async fn retries_transient_http_failures_without_requiring_json() {
        for (status, body) in [
            (502, "<html>Bad Gateway</html>"),
            (503, r#"{"code":503,"errorMsg":"unavailable"}"#),
            (500, ""),
            (504, "Gateway Timeout"),
            (408, ""),
            (429, r#"{"errorMsg":"rate limited"}"#),
        ] {
            let error = decode_test_response(status, body).await.unwrap_err();
            assert!(matches!(error, JmError::Http(_)), "{status}: {error:?}");
            assert!(error.is_retryable());
            assert!(error.to_string().contains(&status.to_string()));
            assert!(error.to_string().contains(TEST_URL));
        }
    }

    #[tokio::test]
    async fn permanent_http_failures_preserve_messages_without_requiring_json() {
        for (status, body, message) in [
            (400, r#"{"code":400,"errorMsg":"无效请求"}"#, "无效请求"),
            (400, r#"{"errorMsg":"缺少参数"}"#, "缺少参数"),
            (401, "", "HTTP 401 Unauthorized"),
            (403, "<html>Forbidden</html>", "HTTP 403 Forbidden"),
            (404, "Not Found", "HTTP 404 Not Found"),
        ] {
            let error = decode_test_response(status, body).await.unwrap_err();
            assert!(matches!(&error, JmError::Api(value) if value.contains(message)));
            assert!(!error.is_retryable());
        }
    }

    #[tokio::test]
    async fn successful_http_business_errors_remain_non_retryable() {
        let error = decode_test_response(200, r#"{"code":400,"errorMsg":"密码错误"}"#)
            .await
            .unwrap_err();
        assert!(matches!(&error, JmError::Api(message) if message == "密码错误"));
        assert!(!error.is_retryable());
    }

    #[tokio::test]
    async fn decodes_plain_success_payloads() {
        let value = decode_test_response(200, r#"{"code":200,"data":{"id":123}}"#)
            .await
            .unwrap();
        assert_eq!(value, json!({"id": 123}));
    }

    #[tokio::test]
    async fn decodes_encrypted_success_payloads() {
        let signature = JmRequestSignature::new();
        let payload = json!({"id": 123, "name": "测试"});
        let key = format!(
            "{:x}",
            md5::compute(format!("{}{}", signature.ts, crate::jm::API_SECRET))
        );
        let encrypted = ecb::Encryptor::<aes::Aes256>::new_from_slice(key.as_bytes())
            .unwrap()
            .encrypt_padded_vec_mut::<Pkcs7>(payload.to_string().as_bytes());
        let body = json!({"code": 200, "data": BASE64.encode(encrypted)}).to_string();
        let value = decode_response::<Value>(test_response(200, &body), TEST_URL, &signature)
            .await
            .unwrap();
        assert_eq!(value, payload);
    }

    #[tokio::test]
    async fn malformed_success_payloads_remain_non_retryable() {
        let error = decode_test_response(200, "not JSON").await.unwrap_err();
        assert!(matches!(error, JmError::Payload(_)));
        assert!(!error.is_retryable());
    }

    async fn decode_test_response(status: u16, body: &str) -> JmResult<Value> {
        decode_response(
            test_response(status, body),
            TEST_URL,
            &JmRequestSignature::new(),
        )
        .await
    }

    fn test_response(status: u16, body: &str) -> reqwest::Response {
        axum::http::Response::builder()
            .status(status)
            .body(body.to_string())
            .unwrap()
            .into()
    }
}

// Internal helpers

fn get_or_create_client() -> JmResult<Client> {
    let client = HTTP_CLIENT.get_or_try_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| JmError::Other(format!("Failed to create HTTP client: {e}")))
    })?;
    Ok(client.clone())
}

trait RequestBuilderExt {
    fn apply_request_signature(
        self,
        url: &str,
        signature: &JmRequestSignature,
    ) -> JmResult<RequestBuilder>;
}

impl RequestBuilderExt for RequestBuilder {
    fn apply_request_signature(
        self,
        url: &str,
        signature: &JmRequestSignature,
    ) -> JmResult<RequestBuilder> {
        let mut builder = self
            .header("accept", "application/json")
            .header("token", &signature.token)
            .header("tokenparam", &signature.tokenparam)
            .header("user-agent", USER_AGENT);

        if let Some(host) = extract_host(url) {
            builder = builder.header("Host", host);
        }

        Ok(builder)
    }
}

fn extract_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    code: i32,
    data: Option<serde_json::Value>,
    #[serde(rename = "errorMsg")]
    error_msg: Option<String>,
    #[serde(skip)]
    _phantom: std::marker::PhantomData<T>,
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
    url: &str,
    signature: &JmRequestSignature,
) -> JmResult<T> {
    let status = response.status();
    if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
        return Err(JmError::Http(format!("HTTP {status}: {url}")));
    }

    let body = response
        .text()
        .await
        .map_err(|e| JmError::Network(e.to_string()))?;

    let body = body.trim();
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|envelope| envelope.get("errorMsg")?.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {status}: {url}"));
        return Err(JmError::Api(message));
    }

    if body.is_empty() {
        return Err(JmError::Empty);
    }

    let envelope: ApiResponse<T> =
        serde_json::from_str(body).map_err(|e| JmError::Payload(e.to_string()))?;

    if envelope.code != 200 {
        return Err(JmError::Api(
            envelope
                .error_msg
                .unwrap_or_else(|| format!("API error code: {}", envelope.code)),
        ));
    }

    let data = envelope.data.ok_or(JmError::MissingData)?;

    match data {
        serde_json::Value::String(encrypted) => {
            let decrypted = crypto::decrypt_data(&encrypted, &signature.ts)?;
            serde_json::from_str(&decrypted).map_err(|e| JmError::Payload(e.to_string()))
        }
        value => serde_json::from_value(value).map_err(|e| JmError::Payload(e.to_string())),
    }
}
