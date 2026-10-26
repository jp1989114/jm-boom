// Validate ID text without imposing an integer range or removing leading zeroes.
pub(super) fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

// Collection pagination is one-based; reader page indexes use a different rule.
pub(super) fn is_positive_page(page: u32) -> bool {
    page > 0
}
