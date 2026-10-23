mod comments;
mod home;
mod search;

pub(crate) use comments::ComicComments;
pub(crate) use home::{
    HomeFeed, HomeSectionList, HomeSectionMode, HomeSectionRequest, WeekFilters, WeekItems,
};
pub(crate) use search::{ComicSearch, ComicSearchRequest};

use crate::{
    domain::{comic::ComicDetail, reader::ChapterManifest},
    endpoint::{request_with_failover, EndpointManager},
    expiring_cache::ExpiringCache,
    jm::{JmClient, JmResult},
    keyed_lock::KeyedLock,
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

const SOURCE_PAGE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SOURCE_PAGE_CACHE_MAX_ENTRIES: usize = 128;

#[derive(Clone)]
pub struct ComicService {
    jm: Arc<JmClient>,
    endpoints: Arc<EndpointManager>,
    source_pages: Arc<ExpiringCache<Arc<home::SourcePage>>>,
    source_page_locks: KeyedLock,
}

impl ComicService {
    pub fn new(jm: Arc<JmClient>, endpoints: Arc<EndpointManager>) -> Self {
        Self {
            jm,
            endpoints,
            source_pages: Arc::new(ExpiringCache::new(
                SOURCE_PAGE_CACHE_TTL,
                SOURCE_PAGE_CACHE_MAX_ENTRIES,
            )),
            source_page_locks: KeyedLock::new(),
        }
    }

    pub async fn get_comic_detail(&self, comic_id: String) -> JmResult<ComicDetail> {
        self.with_failover(move |client, endpoint| {
            let comic_id = comic_id.clone();
            Box::pin(async move { client.get_comic_detail(endpoint, &comic_id).await })
        })
        .await
    }

    pub async fn get_chapter(&self, chapter_id: String) -> JmResult<ChapterManifest> {
        self.with_failover(move |client, endpoint| {
            let chapter_id = chapter_id.clone();
            Box::pin(async move { client.get_chapter(endpoint, &chapter_id).await })
        })
        .await
    }

    async fn with_failover<T, F>(&self, operation: F) -> JmResult<T>
    where
        F: for<'a> Fn(
            &'a JmClient,
            &'a str,
        ) -> Pin<Box<dyn Future<Output = JmResult<T>> + Send + 'a>>,
    {
        request_with_failover(&self.jm, &self.endpoints, operation)
            .await
            .map(|(_, value)| value)
    }
}
