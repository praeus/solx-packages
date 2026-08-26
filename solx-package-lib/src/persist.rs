//! Save a document to solx-server via `PUT /docs/{path}/{name}`. The
//! reference is the URL; the body is `{typeRef, contents}`, and the
//! response is the saved `Document`.
//!
//! One primitive shared by every package that persists extraction output
//! (`solx-media`, `solx-omniparse`, as of this writing) — the HTTP mechanics
//! (URL building, percent-encoding, bearer auth, response parsing) were
//! previously duplicated verbatim between them; only the document shape
//! (`type_ref` and `contents`) actually differs per package.

use serde_json::{json, Value};

/// PUT one document to `{server_url}/docs{path}/{name}`. Returns the saved
/// `(path, name)` on 2xx — read back from the response body fields
/// `saved_path`/`saved_name` (falling back to the ones passed in, in case a
/// future server response shape omits them) — or the server error body on
/// non-2xx.
pub async fn put_document(
    client: &reqwest::Client,
    server_url: &str,
    token: &str,
    path: &str,
    name: &str,
    type_ref: &str,
    contents: Value,
) -> Result<(String, String), String> {
    let body = json!({ "typeRef": type_ref, "contents": contents });

    let url = format!(
        "{}/docs{}/{}",
        server_url.trim_end_matches('/'),
        encode_path(path),
        encode_segment(name),
    );

    let resp = client
        .put(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("doc save request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("doc save returned HTTP {status}: {body}"));
    }

    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse doc save response: {e}"))?;

    let saved_path = parsed
        .get("saved_path")
        .or_else(|| parsed.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or(path)
        .to_string();
    let saved_name = parsed
        .get("saved_name")
        .or_else(|| parsed.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(name)
        .to_string();

    Ok((saved_path, saved_name))
}

/// Percent-encode each segment of a `/`-separated path, keeping the
/// separators. Entity names may legally contain characters that are not
/// URL-safe, and the reference now travels in the URL.
fn encode_path(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| format!("/{}", encode_segment(s)))
        .collect()
}

fn encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Captures one PUT request (request line, auth header, body) and
    /// answers it with `response_body`.
    async fn start_capture_server(
        response_body: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<(String, String, String)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let request_line = req.lines().next().unwrap_or("").to_string();
            let auth = req
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .unwrap_or("")
                .to_string();
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let _ = tx.send((request_line, auth, body));
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(resp.as_bytes()).await;
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    async fn puts_to_the_encoded_url_with_bearer_and_type_ref_body() {
        let (base, rx) = start_capture_server(r#"{"saved_path":"/media/image-text","saved_name":"cat photo"}"#).await;
        let client = reqwest::Client::new();

        let (path, name) = put_document(
            &client,
            &base,
            "tok-123",
            "/media/image-text",
            "cat photo",
            "/packages/solx-media/MediaDocument",
            json!({"kind": "image-text"}),
        )
        .await
        .unwrap();

        assert_eq!(path, "/media/image-text");
        assert_eq!(name, "cat photo");

        let (request_line, auth, body) = tokio::time::timeout(std::time::Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(request_line.starts_with("PUT /docs/media/image-text/cat%20photo"), "{request_line}");
        assert!(auth.to_ascii_lowercase().contains("bearer tok-123"), "{auth}");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["typeRef"], "/packages/solx-media/MediaDocument");
        assert_eq!(parsed["contents"]["kind"], "image-text");
    }

    #[tokio::test]
    async fn falls_back_to_the_passed_in_path_and_name_when_the_response_omits_them() {
        let (base, _rx) = start_capture_server(r#"{}"#).await;
        let client = reqwest::Client::new();

        let (path, name) =
            put_document(&client, &base, "tok", "/media/audio-transcript", "clip", "/packages/solx-media/MediaDocument", json!({}))
                .await
                .unwrap();

        assert_eq!(path, "/media/audio-transcript");
        assert_eq!(name, "clip");
    }

    #[tokio::test]
    async fn a_non_2xx_status_surfaces_the_response_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 8192];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"kind":"validation","message":"bad type_ref"}"#;
            let resp = format!(
                "HTTP/1.1 422 Unprocessable Entity\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes()).await;
        });
        let client = reqwest::Client::new();

        let err = put_document(&client, &format!("http://{addr}"), "tok", "/media/x", "y", "/t", json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("422"), "{err}");
        assert!(err.contains("bad type_ref"), "{err}");
    }

    #[test]
    fn encode_segment_percent_encodes_spaces_and_reserved_chars() {
        assert_eq!(encode_segment("cat photo"), "cat%20photo");
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        assert_eq!(encode_segment("safe-Name_1.0~"), "safe-Name_1.0~");
    }

    #[test]
    fn encode_path_keeps_separators_and_encodes_each_segment() {
        assert_eq!(encode_path("/media/image text"), "/media/image%20text");
    }
}
