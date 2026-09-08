pub fn build_request_id(product_id: &str, tag: &str) -> String {
    format!("{}-{}-{}", product_id, tag, uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::build_request_id;

    #[test]
    fn request_id_contains_product_and_tag() {
        let value = build_request_id("cloud-api-hub", "oauth");
        assert!(value.starts_with("cloud-api-hub-oauth-"));
        assert!(value.len() > "cloud-api-hub-oauth-".len());
    }
}
