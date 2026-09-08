use super::*;

#[tokio::test]
async fn rule_component_binding_discovery_returns_rule_semantics_with_field_slots() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("");

            let body = match step {
                0 => {
                    assert!(
                        first_line.starts_with(
                            "GET /wp-json/wptsall/v2/route_test/client/site-relations"
                        ),
                        "expected site-relations fetch, got: {}",
                        first_line
                    );
                    serde_json::to_string(&json!({
                        "relations": [
                            {
                                "id": 12,
                                "source_site_id": 1,
                                "source_lang": "en_US",
                                "target_site_id": 2,
                                "target_site_type": "post",
                                "target_lang": "zh_CN",
                                "sync_mode": "manual",
                                "media_handling": "copy",
                                "template": "",
                                "models": [
                                    {
                                        "model_id": 99,
                                        "plugin_slug": "woocommerce",
                                        "plugin_name": "WooCommerce",
                                        "post_types": ["shop_order"],
                                        "taxonomies": []
                                    }
                                ]
                            }
                        ]
                    }))
                    .unwrap()
                }
                1 => {
                    assert!(
                        first_line.starts_with(
                            "GET /wp-json/wptsall/v2/route_test/client/rules?relation_id=12"
                        ),
                        "expected rules fetch, got: {}",
                        first_line
                    );
                    serde_json::to_string(&json!({
                        "rules": [
                            {
                                "id": 128,
                                "model_id": 99,
                                "name": "Order Email Template",
                                "data_type": "post",
                                "object_name": "shop_order",
                                "field_capabilities": {},
                                "translate_fields": ["subject", "body_html"],
                                "related_taxonomies": [],
                                "field_content_formats": {
                                    "subject": "plain_text",
                                    "body_html": "rich_html"
                                },
                                "field_storage_map": {
                                    "subject": "post_title",
                                    "body_html": "post_content"
                                },
                                "source_group": "message_template",
                                "routing_profile": "notification_email",
                                "delivery_target": "message_template_writeback",
                                "required_component_slots": ["plain_text", "rich_html"],
                                "required_content_formats": ["plain_text", "rich_html"],
                                "field_source_roles": {
                                    "subject": "message_subject",
                                    "body_html": "message_body_html"
                                }
                            }
                        ]
                    }))
                    .unwrap()
                }
                _ => unreachable!(),
            };

            let signing_key = crate::crypto::derive_signing_key("wp-token-test");
            let sig = crate::crypto::compute_request_signature(&signing_key, &body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sig,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
    {
        let mut guard = state.lock().await;
        guard.domains = vec![DomainStatusItem {
            api_base_url: format!("http://{}", upstream_addr),
            site_status: "active".to_string(),
            route_secret: Some("route_test".to_string()),
            max_relations: Some(10),
            plan_expires_at: None,
        }];
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", upstream_addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp-token-test".to_string(),
                route_secret: "route_test".to_string(),
            },
        );
    }

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_rule_component_binding_discovery(&mut socket, &state)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "rule discovery should succeed, got response: {}",
        response
    );
    assert_eq!(
        payload["data"]["summary"]["domains_checked"],
        json!(1),
        "payload={}",
        payload
    );
    assert_eq!(payload["data"]["summary"]["relations_checked"], json!(1));
    assert_eq!(payload["data"]["summary"]["rules_checked"], json!(1));
    assert_eq!(payload["data"]["summary"]["fields_checked"], json!(2));
    assert_eq!(payload["data"]["items"][0]["rule_id"], json!(128));
    assert_eq!(
        payload["data"]["items"][0]["source_group"],
        json!("message_template")
    );
    assert_eq!(
        payload["data"]["items"][0]["routing_profile"],
        json!("notification_email")
    );
    assert_eq!(
        payload["data"]["items"][0]["fields"][0]["required_slot_key"],
        json!("rich_html")
    );
    assert_eq!(
        payload["data"]["items"][0]["fields"][1]["required_slot_key"],
        json!("plain_text")
    );
}
