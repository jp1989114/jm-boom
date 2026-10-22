use super::{
    current_china_weekday,
    payload::{CategoryPayload, ComicListPayload, PromotePayload, SectionPayload, WeeklyPayload},
    ComicService, HomeSectionMode, HomeSectionRequest, SourcePage, PAGE_SIZE, PROMOTE_PAGE_SIZE,
};
use crate::{
    endpoint::request_with_failover,
    jm::{JmError, JmResult},
};
use serde::de::DeserializeOwned;
use std::sync::Arc;

impl ComicService {
    pub(super) async fn request_section_list(
        &self,
        request: &HomeSectionRequest,
        page: u32,
    ) -> JmResult<SectionPayload> {
        let start = page.saturating_sub(1) as usize * PAGE_SIZE;
        match request.mode {
            HomeSectionMode::Promote => self.promote_section(request, page).await,
            HomeSectionMode::Weekly => self.weekly_section(request, start).await,
            HomeSectionMode::Latest => self.latest_section(start).await,
            HomeSectionMode::Ranking => self.ranking_section(request, start).await,
        }
    }

    async fn promote_section(
        &self,
        request: &HomeSectionRequest,
        page: u32,
    ) -> JmResult<SectionPayload> {
        let id = request
            .section_id
            .parse::<u32>()
            .ok()
            .or_else(|| request.filter_value.parse::<u32>().ok())
            .unwrap_or_default();
        let source_page = page.saturating_sub(1);
        let payload: PromotePayload = self
            .with_failover(move |client, endpoint| {
                Box::pin(async move {
                    client
                        .get(
                            endpoint,
                            "promote_list",
                            &[("id", id.to_string()), ("page", source_page.to_string())],
                        )
                        .await
                })
            })
            .await?;
        let count = payload.list.len();
        let loaded_count = source_page as usize * PROMOTE_PAGE_SIZE + count;

        Ok(SectionPayload {
            page_size: PROMOTE_PAGE_SIZE as u32,
            total: payload.total,
            has_more: count >= PROMOTE_PAGE_SIZE
                && (payload.total == 0 || loaded_count < payload.total as usize),
            items: payload.list,
        })
    }

    async fn weekly_section(
        &self,
        request: &HomeSectionRequest,
        start: usize,
    ) -> JmResult<SectionPayload> {
        let request_page = (start / 40) as u32 + 1;
        let offset = start % 40;
        let date = request
            .week
            .parse::<u32>()
            .unwrap_or_else(|_| current_china_weekday());
        let category = if request.category.is_empty() {
            "all".to_string()
        } else {
            request.category.clone()
        };
        let payload = self
            .fetch_source_page(
                "serialization",
                vec![
                    ("date", date.to_string()),
                    ("type", category),
                    ("page", request_page.to_string()),
                ],
                |payload: WeeklyPayload| SourcePage {
                    items: payload.list,
                    total: 0,
                },
            )
            .await?;
        let count = payload.items.len();

        Ok(SectionPayload {
            page_size: PAGE_SIZE as u32,
            total: 0,
            has_more: count > offset + PAGE_SIZE || count >= 40,
            items: payload
                .items
                .iter()
                .skip(offset)
                .take(PAGE_SIZE)
                .cloned()
                .collect(),
        })
    }

    async fn latest_section(&self, start: usize) -> JmResult<SectionPayload> {
        let request_page = (start / 80) as u32;
        let offset = start % 80;
        let payload = self
            .fetch_source_page(
                "latest",
                vec![("page", request_page.to_string())],
                |items: Vec<ComicListPayload>| SourcePage { items, total: 0 },
            )
            .await?;
        let count = payload.items.len();

        Ok(SectionPayload {
            page_size: PAGE_SIZE as u32,
            total: 0,
            has_more: count > offset + PAGE_SIZE || count >= 80,
            items: payload
                .items
                .iter()
                .skip(offset)
                .take(PAGE_SIZE)
                .cloned()
                .collect(),
        })
    }

    async fn ranking_section(
        &self,
        request: &HomeSectionRequest,
        start: usize,
    ) -> JmResult<SectionPayload> {
        let request_page = (start / 80) as u32;
        let offset = start % 80;
        let category = if request.category.is_empty() {
            "latest".to_string()
        } else {
            request.category.clone()
        };
        let order = if request.order.is_empty() {
            "new".to_string()
        } else {
            request.order.clone()
        };
        let payload = self
            .fetch_source_page(
                "categories/filter",
                vec![
                    ("page", request_page.to_string()),
                    ("c", category),
                    ("o", order),
                ],
                |payload: CategoryPayload| SourcePage {
                    items: payload.content,
                    total: payload.total,
                },
            )
            .await?;
        let count = payload.items.len();

        Ok(SectionPayload {
            page_size: PAGE_SIZE as u32,
            total: payload.total,
            has_more: count > offset + PAGE_SIZE
                || (payload.total > 0 && start + PAGE_SIZE < payload.total as usize),
            items: payload
                .items
                .iter()
                .skip(offset)
                .take(PAGE_SIZE)
                .cloned()
                .collect(),
        })
    }

    async fn fetch_source_page<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &'static str,
        query: Vec<(&'static str, String)>,
        map: fn(T) -> SourcePage,
    ) -> JmResult<Arc<SourcePage>> {
        let auth = self.jm.request_auth().await;
        let scope = serde_json::to_string(&(path, &query, auth.generation))
            .map_err(|error| JmError::Payload(error.to_string()))?;
        if let Some(page) = self.cached_source_page(&scope).await {
            return Ok(page);
        }

        // Keep failover waiters together, even when the first attempt changes the active endpoint.
        let _guard = self.source_page_locks.lock(&scope).await;
        if let Some(page) = self.cached_source_page(&scope).await {
            return Ok(page);
        }

        let (endpoint, page) =
            request_with_failover(&self.jm, &self.endpoints, move |client, endpoint| {
                let query = query.clone();
                let auth = auth.clone();
                Box::pin(async move {
                    client
                        .get_with_auth::<T>(endpoint, path, &query, &auth)
                        .await
                        .map(map)
                        .map(Arc::new)
                })
            })
            .await?;
        self.source_pages
            .insert(format!("{endpoint}|{scope}"), page.clone())
            .await;
        Ok(page)
    }

    async fn cached_source_page(&self, scope: &str) -> Option<Arc<SourcePage>> {
        let endpoint = self
            .endpoints
            .request_candidates()
            .await
            .into_iter()
            .next()?;
        // A cache hit must not be reported as a network success or alter probe latency.
        self.source_pages.get(&format!("{endpoint}|{scope}")).await
    }
}
