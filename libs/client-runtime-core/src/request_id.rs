pub fn build_request_id(product_id: &str, tag: &str) -> String {
    format!("{}-{}-{}", product_id, tag, uuid::Uuid::new_v4())
}
