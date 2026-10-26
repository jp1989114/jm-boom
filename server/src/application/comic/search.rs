use super::ComicService;
use crate::{domain::comic::ComicSummary, jm::JmResult};

const SEARCH_PAGE_SIZE: u32 = 80;

pub(crate) struct ComicSearchRequest {
    pub(crate) keyword: String,
    pub(crate) page: u32,
    pub(crate) sort_by: u8,
}

pub(crate) struct ComicSearch {
    pub(crate) page: u32,
    pub(crate) pages: u32,
    pub(crate) total: u32,
    pub(crate) has_reached_max: bool,
    pub(crate) items: Vec<ComicSummary>,
}

impl ComicService {
    pub async fn search_comics(&self, request: ComicSearchRequest) -> JmResult<ComicSearch> {
        let keyword = request.keyword.trim().to_string();
        let page = request.page.max(1);
        if keyword.is_empty() {
            return Ok(search_result(page, 0, Vec::new()));
        }

        if page == 1 && is_comic_id(&keyword) {
            if let Ok(detail) = self.get_comic_detail(keyword.clone()).await {
                return Ok(search_result(
                    page,
                    1,
                    vec![ComicSummary::from_detail(detail)],
                ));
            }
        }

        let order = match request.sort_by {
            2 => "mv",
            3 => "mp",
            4 => "tf",
            _ => "mr",
        }
        .to_string();
        let result = self
            .with_failover(move |client, endpoint| {
                let keyword = keyword.clone();
                let order = order.clone();
                Box::pin(async move { client.search(endpoint, &keyword, page, &order).await })
            })
            .await?;

        if result.content.is_empty() {
            if let Some(redirect_id) = result.redirect_aid.filter(|id| is_comic_id(id)) {
                let detail = self.get_comic_detail(redirect_id).await?;
                return Ok(search_result(
                    page,
                    1,
                    vec![ComicSummary::from_detail(detail)],
                ));
            }
        }

        let total = result.total;
        let items = result
            .content
            .into_iter()
            .map(|comic| {
                ComicSummary::new(
                    comic.id,
                    comic.name,
                    comic.author,
                    comic.description,
                    comic.image,
                    comic.tags,
                )
            })
            .collect();
        Ok(search_result(page, total, items))
    }
}

fn search_result(page: u32, total: u32, items: Vec<ComicSummary>) -> ComicSearch {
    let pages = total.div_ceil(SEARCH_PAGE_SIZE);
    let item_count = items.len() as u32;
    // Upstream may omit the total; fall back to the current page length so a
    // full page does not look like the last one.
    let loaded_count = u64::from(page.saturating_sub(1))
        .saturating_mul(u64::from(SEARCH_PAGE_SIZE))
        .saturating_add(u64::from(item_count));
    let has_reached_max =
        item_count < SEARCH_PAGE_SIZE || (total > 0 && loaded_count >= u64::from(total));
    ComicSearch {
        page,
        pages,
        total,
        has_reached_max,
        items,
    }
}

fn is_comic_id(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{search_result, SEARCH_PAGE_SIZE};
    use crate::domain::comic::ComicSummary;

    #[test]
    fn keeps_a_full_page_open_when_upstream_omits_the_total() {
        let result = search_result(1, 0, comics(SEARCH_PAGE_SIZE as usize));
        assert!(!result.has_reached_max);
    }

    #[test]
    fn stops_on_a_short_page_when_upstream_omits_the_total() {
        let result = search_result(1, 0, comics(SEARCH_PAGE_SIZE as usize - 1));
        assert!(result.has_reached_max);
    }

    #[test]
    fn stops_on_an_empty_page() {
        let result = search_result(1, 0, Vec::new());
        assert!(result.has_reached_max);
    }

    #[test]
    fn uses_the_total_when_it_is_available() {
        let first = search_result(1, 81, comics(SEARCH_PAGE_SIZE as usize));
        assert_eq!(first.pages, 2);
        assert!(!first.has_reached_max);

        let last = search_result(2, 81, comics(1));
        assert_eq!(last.pages, 2);
        assert!(last.has_reached_max);
    }

    #[test]
    fn large_page_numbers_do_not_overflow_the_loaded_count() {
        let result = search_result(u32::MAX, u32::MAX, comics(SEARCH_PAGE_SIZE as usize));
        assert!(result.has_reached_max);
    }

    fn comics(count: usize) -> Vec<ComicSummary> {
        (0..count)
            .map(|index| {
                ComicSummary::new(
                    index.to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    Vec::new(),
                )
            })
            .collect()
    }
}
