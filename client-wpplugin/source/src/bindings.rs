use std::collections::HashSet;
use std::path::Path;

use crate::types::*;

mod binding_v2;
mod components;
mod crypto;
mod domain_tokens;
mod integrations;
mod task_rules;

pub(crate) fn load_component_bindings(path: &str) -> anyhow::Result<ComponentBindingsDoc> {
    components::load_component_bindings(path)
}

pub(crate) fn save_component_bindings(
    path: &str,
    doc: &ComponentBindingsDoc,
) -> anyhow::Result<()> {
    components::save_component_bindings(path, doc)
}

pub(crate) fn load_domain_token_bindings(path: &str) -> anyhow::Result<DomainTokenBindingsDoc> {
    domain_tokens::load_domain_token_bindings(path)
}

pub(crate) fn save_domain_token_bindings(
    path: &str,
    doc: &DomainTokenBindingsDoc,
) -> anyhow::Result<()> {
    domain_tokens::save_domain_token_bindings(path, doc)
}

pub(crate) fn normalize_api_base_url_key(input: &str) -> String {
    domain_tokens::normalize_api_base_url_key(input)
}

/// Normalize a domain input to a canonical `scheme://host` key used in
/// domain-token-bindings (version 2 format).
///
/// - Strips any path/query/fragment (only scheme + host are kept)
/// - Defaults to `https` when no scheme is present
/// - Lowercases the result
///
/// Examples:
///   "https://blog.wpmm.cc/wp-json/wptsall/v2/SECRET/client" → "https://blog.wpmm.cc"
///   "https://blog.wpmm.cc"                                   → "https://blog.wpmm.cc"
///   "blog.wpmm.cc"                                           → "https://blog.wpmm.cc"
///   "http://127.0.0.1:9090/path"                             → "http://127.0.0.1:9090"
pub(crate) fn normalize_domain_base(url: &str) -> String {
    domain_tokens::normalize_domain_base(url)
}

pub(crate) fn has_any_domain_token_bindings(doc: &DomainTokenBindingsDoc) -> bool {
    domain_tokens::has_any_domain_token_bindings(doc)
}

pub(crate) fn resolve_wp_client_token_for_domain(
    api_base_url: &str,
    bindings_doc: &DomainTokenBindingsDoc,
    fallback_wp_client_token: &str,
) -> Option<String> {
    domain_tokens::resolve_wp_client_token_for_domain(
        api_base_url,
        bindings_doc,
        fallback_wp_client_token,
    )
}

/// Resolve route_secret for a domain from domain-token-bindings.
/// Returns None if no route_secret is configured (backward-compatible).
pub(crate) fn resolve_route_secret_for_domain(
    api_base_url: &str,
    bindings_doc: &DomainTokenBindingsDoc,
) -> Option<String> {
    domain_tokens::resolve_route_secret_for_domain(api_base_url, bindings_doc)
}

/// Given a domain base URL (scheme://host) and a route_secret, construct
/// the WP REST API base URL for this site.
///
/// Returns `None` if route_secret is empty (caller should fall back to the
/// server-provided api_base_url for backward compatibility).
pub(crate) fn build_wp_base_url(domain_base: &str, route_secret: &str) -> Option<String> {
    domain_tokens::build_wp_base_url(domain_base, route_secret)
}

pub(crate) fn domain_token_binding_status_items(
    doc: &DomainTokenBindingsDoc,
) -> Vec<DomainTokenBindingStatusItem> {
    domain_tokens::domain_token_binding_status_items(doc)
}

pub(crate) fn domain_token_binding_local_sites(
    doc: &DomainTokenBindingsDoc,
) -> Vec<DomainStatusItem> {
    domain_tokens::domain_token_binding_local_sites(doc)
}

pub(crate) fn parse_task_type_binding_key(raw: &str) -> Option<String> {
    task_rules::parse_task_type_binding_key(raw)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_content_format_binding_key(raw: &str) -> Option<String> {
    task_rules::parse_content_format_binding_key(raw)
}

pub(crate) fn parse_rule_component_slot_binding_key(raw: &str) -> Option<String> {
    task_rules::parse_rule_component_slot_binding_key(raw)
}

pub(crate) fn normalize_rule_component_bindings_doc(
    doc: &RuleComponentBindingsDoc,
) -> RuleComponentBindingsDoc {
    task_rules::normalize_rule_component_bindings_doc(doc)
}

/// Resolve one local-dev binding entry from domain-token-bindings.
///
/// Local-dev slot should use a binding key that is not already occupied by
/// server-managed domains. This allows one extra local site without polluting
/// official domain bindings.
///
/// Returns `(api_base_url, wp_client_token, route_secret)`.
pub(crate) fn resolve_local_dev_binding(
    bindings_doc: &DomainTokenBindingsDoc,
    occupied_domain_bases: &HashSet<String>,
) -> Option<(String, String, Option<String>)> {
    domain_tokens::resolve_local_dev_binding(bindings_doc, occupied_domain_bases)
}

pub(crate) fn parse_business_line_key(raw: &str) -> Option<String> {
    task_rules::parse_business_line_key(raw)
}

pub(crate) fn parse_business_line_task_type_binding_key(raw: &str) -> Option<String> {
    task_rules::parse_business_line_task_type_binding_key(raw)
}

pub(crate) fn load_task_type_component_bindings(
    path: &str,
) -> anyhow::Result<TaskTypeComponentBindingsDoc> {
    task_rules::load_task_type_component_bindings(path)
}

pub(crate) fn save_task_type_component_bindings(
    path: &str,
    doc: &TaskTypeComponentBindingsDoc,
) -> anyhow::Result<()> {
    task_rules::save_task_type_component_bindings(path, doc)
}

pub(crate) fn task_type_component_binding_status_items(
    doc: &TaskTypeComponentBindingsDoc,
) -> Vec<TaskTypeComponentBindingStatusItem> {
    task_rules::task_type_component_binding_status_items(doc)
}

pub(crate) fn rule_component_binding_status_items(
    doc: &RuleComponentBindingsDoc,
) -> Vec<RuleComponentBindingStatusItem> {
    task_rules::rule_component_binding_status_items(doc)
}

// ---------------------------------------------------------------------------
// Rule Component Bindings
// ---------------------------------------------------------------------------

pub(crate) fn load_rule_component_bindings(path: &str) -> anyhow::Result<RuleComponentBindingsDoc> {
    task_rules::load_rule_component_bindings(path)
}

pub(crate) fn save_rule_component_bindings(
    path: &str,
    doc: &RuleComponentBindingsDoc,
) -> anyhow::Result<()> {
    task_rules::save_rule_component_bindings(path, doc)
}

/// Resolve component ID using rule→plugin→relation→global priority chain.
pub(crate) fn resolve_component_id_from_rule_bindings<'a>(
    doc: &'a RuleComponentBindingsDoc,
    rule_id: Option<u64>,
    relation_id: Option<u64>,
    plugin_slug: Option<&str>,
    task_type: &str,
    content_format: Option<&str>,
) -> Option<&'a str> {
    task_rules::resolve_component_id_from_rule_bindings(
        doc,
        rule_id,
        relation_id,
        plugin_slug,
        task_type,
        content_format,
    )
}

pub(crate) use binding_v2::{
    component_semantic_key, migrate_rule_bindings_v1_to_v2, numeric_scope_keys,
    pack_has_cross_site_numeric_risk, relation_semantic_key, resolve_rule_bindings_with_context,
    rule_semantic_key, sanitize_rule_bindings_for_public_pack, site_key_from_origin,
    strip_numeric_maps_on_import, BindingDiscoveryIndex, BindingResolveContext,
    BindingResolveOutcome, BindingResolveResult, DiscoveredRelationIdentity,
    DiscoveredRuleIdentity, MigrationReport, SiteDiscoveryIndex, BINDINGS_DOC_VERSION_V2,
};

// ---------------------------------------------------------------------------
// Vendor Keys
// ---------------------------------------------------------------------------

pub(crate) fn load_vendor_keys(path: &str) -> anyhow::Result<VendorKeysDoc> {
    integrations::load_vendor_keys(path)
}

pub(crate) fn save_vendor_keys(path: &str, doc: &VendorKeysDoc) -> anyhow::Result<()> {
    integrations::save_vendor_keys(path, doc)
}

// ---------------------------------------------------------------------------
// Vendor OAuth
// ---------------------------------------------------------------------------

pub(crate) fn load_vendor_oauth(path: &str) -> anyhow::Result<VendorOAuthDoc> {
    integrations::load_vendor_oauth(path)
}

pub(crate) fn save_vendor_oauth(path: &str, doc: &VendorOAuthDoc) -> anyhow::Result<()> {
    integrations::save_vendor_oauth(path, doc)
}

// ---------------------------------------------------------------------------
// Proxy Profiles
// ---------------------------------------------------------------------------

pub(crate) fn load_proxy_profiles(path: &str) -> anyhow::Result<ProxyProfilesDoc> {
    integrations::load_proxy_profiles(path)
}

pub(crate) fn save_proxy_profiles(path: &str, doc: &ProxyProfilesDoc) -> anyhow::Result<()> {
    integrations::save_proxy_profiles(path, doc)
}

// ---------------------------------------------------------------------------
// Components Local
// ---------------------------------------------------------------------------

pub(crate) fn load_components_local(path: &str) -> anyhow::Result<ComponentsLocalDoc> {
    components::load_components_local(path)
}

pub(crate) fn save_components_local(path: &str, doc: &ComponentsLocalDoc) -> anyhow::Result<()> {
    components::save_components_local(path, doc)
}

#[allow(dead_code)]
pub(crate) fn bindings_secret() -> Option<String> {
    crypto::bindings_secret()
}

fn load_encrypted_or_plain(file_path: &Path) -> anyhow::Result<String> {
    crypto::load_encrypted_or_plain(file_path)
}

pub(crate) fn encrypt_for_save(plain_json: &str) -> anyhow::Result<Vec<u8>> {
    crypto::encrypt_for_save(plain_json)
}

pub(crate) fn decrypt_from_bytes(data: &[u8]) -> anyhow::Result<String> {
    crypto::decrypt_from_bytes(data)
}

#[cfg(test)]
mod tests;
