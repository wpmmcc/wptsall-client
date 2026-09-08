use anyhow::{anyhow, Context};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(super) fn parse_query_string(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = urldecode(key);
            let value = urldecode(value);
            params.insert(key, value);
        }
    }
    params
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub(super) async fn read_simple_http_request(
    socket: &mut TcpStream,
) -> anyhow::Result<Option<(String, String, String, HashMap<String, String>, Vec<u8>)>> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let mut header_end: Option<usize> = None;

    loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if header_end.is_none() {
            if let Some(idx) = find_header_end(&buf) {
                header_end = Some(idx);
                break;
            }
        }
        if buf.len() > 128 * 1024 {
            return Err(anyhow!("request too large"));
        }
    }

    let header_end = header_end.ok_or_else(|| anyhow!("invalid http request"))?;
    let head = String::from_utf8(buf[..header_end].to_vec())
        .with_context(|| "http header is not utf-8".to_string())?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target_full = parts.next().unwrap_or("/").to_string();
    let (target, query_string) = if let Some(idx) = target_full.find('?') {
        (
            target_full[..idx].to_string(),
            target_full[idx + 1..].to_string(),
        )
    } else {
        (target_full.clone(), String::new())
    };
    if method.is_empty() {
        return Err(anyhow!("missing method"));
    }

    let mut content_length = 0usize;
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name_lower = name.trim().to_lowercase();
            let value_trimmed = value.trim().to_string();
            if name_lower == "content-length" {
                content_length = value_trimmed.parse::<usize>().unwrap_or(0).min(1024 * 1024);
            }
            headers.insert(name_lower, value_trimmed);
        }
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 2 * 1024 * 1024 {
            return Err(anyhow!("request body too large"));
        }
    }
    let body = if content_length == 0 || buf.len() <= body_start {
        Vec::new()
    } else {
        buf[body_start..(body_start + content_length).min(buf.len())].to_vec()
    };
    Ok(Some((method, target, query_string, headers, body)))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Serve the main SPA HTML with strict Content-Security-Policy.
/// All JS and CSS are inlined by vite-plugin-singlefile, so only 'unsafe-inline' is needed.
pub(super) async fn write_html_page_response(
    socket: &mut TcpStream,
    body: &[u8],
) -> anyhow::Result<()> {
    let csp = "default-src 'self'; \
                script-src 'unsafe-inline'; \
                style-src 'unsafe-inline'; \
                img-src 'self' data: blob:; \
                connect-src 'self'; \
                font-src 'self' data:; \
                worker-src 'none'; \
                frame-src 'none'; \
                object-src 'none'";
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Content-Security-Policy: {}\r\n\r\n",
        body.len(),
        csp
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

pub(super) async fn write_http_response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}
