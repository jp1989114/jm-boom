use super::{
    media::cover_url,
    validation::{is_numeric_id, is_positive_page},
    CollectionListQuery, COLLECTION_PAGE_SIZE,
};
use crate::{application::FavoriteInput, http_error::HttpError, AppState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteListResponse {
    items: Vec<FavoriteResponse>,
    total: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FavoriteResponse {
    id: String,
    title: String,
    author: String,
    description: String,
    image: String,
    tags: Vec<String>,
    favorited_at: i64,
}

pub async fn list(
    State(app): State<AppState>,
    Query(query): Query<CollectionListQuery>,
) -> Result<Json<FavoriteListResponse>, HttpError> {
    validate_page(query.page)?;
    favorite_list(&app, query.page).await.map(Json)
}

pub async fn upsert(
    State(app): State<AppState>,
    Path(comic_id): Path<String>,
    Json(input): Json<FavoriteInput>,
) -> Result<Json<FavoriteResponse>, HttpError> {
    validate_comic_id(&comic_id)?;
    app.favorites
        .upsert(&comic_id, input)
        .await
        .map(FavoriteResponse::from)
        .map(Json)
        .map_err(internal_error)
}

pub async fn remove(
    State(app): State<AppState>,
    Path(comic_id): Path<String>,
) -> Result<StatusCode, HttpError> {
    validate_comic_id(&comic_id)?;
    app.favorites
        .remove(&comic_id)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn clear(State(app): State<AppState>) -> Result<StatusCode, HttpError> {
    app.favorites.clear().await.map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn favorite_list(app: &AppState, page: u32) -> Result<FavoriteListResponse, HttpError> {
    let (items, total) = app
        .favorites
        .list(page, COLLECTION_PAGE_SIZE)
        .await
        .map_err(internal_error)?;
    let items = items.into_iter().map(FavoriteResponse::from).collect();
    Ok(FavoriteListResponse { items, total })
}

impl From<crate::application::FavoriteItem> for FavoriteResponse {
    fn from(item: crate::application::FavoriteItem) -> Self {
        Self {
            image: cover_url(&item.comic_id, &item.image),
            id: item.comic_id,
            title: item.title,
            author: item.author,
            description: item.description,
            tags: item.tags,
            favorited_at: item.favorited_at,
        }
    }
}

fn validate_comic_id(comic_id: &str) -> Result<(), HttpError> {
    if !is_numeric_id(comic_id) {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            "收藏漫画 ID 必须为数字",
            false,
        ));
    }
    Ok(())
}

fn validate_page(page: u32) -> Result<(), HttpError> {
    if !is_positive_page(page) {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            "页码必须大于 0",
            false,
        ));
    }
    Ok(())
}

fn internal_error(error: anyhow::Error) -> HttpError {
    tracing::error!(%error, "收藏存储操作失败");
    HttpError::internal("收藏存储操作失败")
}

#[cfg(test)]
mod tests {
    use super::{validate_comic_id, validate_page};
    use crate::api::tests::assert_bad_request;

    #[tokio::test]
    async fn preserves_favorite_id_and_list_page_validation() {
        for id in ["0", "000123", "4294967296"] {
            assert!(validate_comic_id(id).is_ok());
        }
        assert!(validate_page(1).is_ok());
        assert!(validate_page(u32::MAX).is_ok());
        assert_bad_request(
            validate_comic_id("１２").unwrap_err(),
            "收藏漫画 ID 必须为数字",
        )
        .await;
        assert_bad_request(validate_page(0).unwrap_err(), "页码必须大于 0").await;
    }
}
