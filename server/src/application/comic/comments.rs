use super::ComicService;
use crate::{
    domain::comic::ComicComment,
    jm::{
        serde_ext::{optional_string_from_any, string_from_any, u32_from_any},
        JmResult,
    },
};
use serde::{Deserialize, Deserializer};

pub(crate) struct ComicComments {
    pub(crate) page: u32,
    pub(crate) total: u32,
    pub(crate) comments: Vec<ComicComment>,
}

impl ComicService {
    pub async fn get_comments(&self, comic_id: String, page: u32) -> JmResult<ComicComments> {
        let page = page.max(1);
        let payload: CommentListPayload = self
            .with_failover(move |client, endpoint| {
                let comic_id = comic_id.clone();
                Box::pin(async move {
                    client
                        .get(
                            endpoint,
                            "forum",
                            &[
                                ("page", page.to_string()),
                                ("aid", comic_id),
                                ("mode", "manhua".to_string()),
                            ],
                        )
                        .await
                })
            })
            .await?;

        Ok(ComicComments {
            page,
            total: payload.total,
            comments: payload.list.into_iter().map(map_comment).collect(),
        })
    }
}

#[derive(Deserialize)]
struct CommentListPayload {
    #[serde(default, deserialize_with = "u32_from_any")]
    total: u32,
    #[serde(default)]
    list: Vec<CommentPayload>,
}

#[derive(Deserialize)]
struct CommentPayload {
    #[serde(default, rename = "AID", deserialize_with = "optional_string_from_any")]
    comic_id: Option<String>,
    #[serde(default, rename = "CID", deserialize_with = "string_from_any")]
    id: String,
    #[serde(default, rename = "UID", deserialize_with = "string_from_any")]
    user_id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    nickname: String,
    #[serde(default, rename = "likes", deserialize_with = "u32_from_any")]
    like_count: u32,
    #[serde(default, rename = "update_at")]
    updated_at: String,
    #[serde(default, rename = "addtime")]
    time: String,
    #[serde(default, rename = "parent_CID", deserialize_with = "string_from_any")]
    parent_id: String,
    #[serde(default)]
    content: String,
    #[serde(default, deserialize_with = "bool_from_any")]
    spoiler: bool,
    #[serde(default, rename = "replys")]
    replies: Vec<CommentPayload>,
}

fn map_comment(payload: CommentPayload) -> ComicComment {
    ComicComment {
        id: payload.id,
        comic_id: payload.comic_id,
        user_id: payload.user_id,
        username: payload.username,
        nickname: payload.nickname,
        content: payload.content,
        like_count: payload.like_count,
        time: payload.time,
        updated_at: payload.updated_at,
        parent_id: payload.parent_id,
        spoiler: payload.spoiler,
        replies: payload.replies.into_iter().map(map_comment).collect(),
    }
}

fn bool_from_any<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Bool(value) => value,
        serde_json::Value::Number(value) => value.as_i64().unwrap_or_default() != 0,
        serde_json::Value::String(value) => matches!(value.as_str(), "1" | "true"),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::{map_comment, ComicService, CommentListPayload};
    use crate::{
        endpoint::{EndpointManager, EndpointProbe},
        jm::{JmClient, JmError},
    };
    use axum::{extract::Query, http::StatusCode, routing::get, Json, Router};
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[tokio::test]
    async fn comments_return_without_waiting_for_image_settings() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let settings_requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/forum",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    assert_eq!(query["aid"], "123");
                    assert_eq!(query["mode"], "manhua");
                    let list = match query["page"].as_str() {
                        "1" => json!([{
                            "CID": 456,
                            "content": "comment",
                            "photo": "avatar.jpg",
                            "replys": [{
                                "CID": 457,
                                "parent_CID": 456,
                                "content": "reply",
                                "photo": "https://cdn.example/reply.jpg"
                            }]
                        }]),
                        "2" => json!([]),
                        page => panic!("unexpected comment page: {page}"),
                    };
                    Json(json!({"code": 200, "data": {"total": 1, "list": list}}))
                }),
            )
            .route(
                "/setting",
                get({
                    let settings_requests = settings_requests.clone();
                    move || {
                        let settings_requests = settings_requests.clone();
                        async move {
                            settings_requests.fetch_add(1, Ordering::SeqCst);
                            std::future::pending::<StatusCode>().await
                        }
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let endpoints = Arc::new(EndpointManager::new(db).await.unwrap());
        endpoints
            .set_probes_for_test(vec![EndpointProbe {
                endpoint,
                available: true,
                latency_ms: Some(1),
                error: None,
            }])
            .await;
        let mut jm = JmClient::new().unwrap();
        jm.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let service = ComicService::new(Arc::new(jm), endpoints);

        let result = tokio::time::timeout(Duration::from_secs(1), async {
            let comments = service.get_comments("123".into(), 1).await?;
            let empty = service.get_comments("123".into(), 2).await?;
            Ok::<_, JmError>((comments, empty))
        })
        .await;
        server.abort();
        let (comments, empty) = result
            .expect("comments must not wait for image settings")
            .expect("fetch comments");

        assert_eq!(settings_requests.load(Ordering::SeqCst), 0);
        assert_eq!(comments.page, 1);
        assert_eq!(comments.total, 1);
        assert_eq!(comments.comments.len(), 1);
        let comment = &comments.comments[0];
        assert_eq!(comment.id, "456");
        assert_eq!(comment.content, "comment");
        assert_eq!(comment.replies.len(), 1);
        assert_eq!(comment.replies[0].id, "457");
        assert_eq!(comment.replies[0].parent_id, "456");
        assert_eq!(comment.replies[0].content, "reply");
        assert_eq!(empty.page, 2);
        assert_eq!(empty.total, 1);
        assert!(empty.comments.is_empty());
    }

    #[test]
    fn preserves_upstream_comment_field_mappings() {
        let payload: CommentListPayload = serde_json::from_value(serde_json::json!({
            "total": "1",
            "list": [{
                "AID": 123,
                "CID": 456,
                "UID": 789,
                "username": "user",
                "nickname": "nickname",
                "content": "comment",
                "likes": "12",
                "addtime": "created",
                "update_at": "updated",
                "parent_CID": 0,
                "photo": "avatar.jpg",
                "spoiler": "1",
                "replys": []
            }]
        }))
        .expect("deserialize upstream comment payload");

        let comment = map_comment(payload.list.into_iter().next().expect("comment"));
        assert_eq!(payload.total, 1);
        assert_eq!(comment.id, "456");
        assert_eq!(comment.comic_id.as_deref(), Some("123"));
        assert_eq!(comment.user_id, "789");
        assert_eq!(comment.like_count, 12);
        assert_eq!(comment.time, "created");
        assert_eq!(comment.updated_at, "updated");
        assert_eq!(comment.parent_id, "0");
        assert!(comment.spoiler);
    }
}
