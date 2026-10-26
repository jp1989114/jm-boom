use super::validation::is_numeric_id;

pub(crate) fn cover_url(comic_id: &str, fallback: &str) -> String {
    if is_numeric_id(comic_id) {
        format!("/api/covers/{comic_id}")
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::cover_url;

    #[test]
    fn preserves_numeric_id_text_and_invalid_id_fallbacks() {
        for id in ["0", "000123", "4294967296"] {
            assert_eq!(cover_url(id, "fallback"), format!("/api/covers/{id}"));
        }
        for id in ["", "+1", "１２", "../123", " 123"] {
            assert_eq!(cover_url(id, "fallback"), "fallback");
        }
    }
}
