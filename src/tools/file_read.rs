use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{PathPolicy, Tool};

pub struct FileReadTool {
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

fn image_ext(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    for ext in ["jpeg", "jpg", "png", "gif", "webp"] {
        if lower.ends_with(&format!(".{ext}")) {
            return Some(match ext {
                "jpg" | "jpeg" => "jpeg",
                other => other,
            });
        }
    }
    None
}

pub(crate) fn b64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let b0 = rem[0] as u32;
        let b1 = if rem.len() > 1 { rem[1] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if rem.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        out.push('=');
    }
    out
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read the contents of a file at the given path. Returns text or, for image files, base64 with mime type."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative path to the file" },
                "offset": { "type": "integer", "minimum": 1, "description": "Start reading from this line (1-indexed)" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to return" }
            },
            "required": ["path"]
        })
    }
    fn require_approval(&self, args: &Value) -> bool {
        args.get("path")
            .and_then(Value::as_str)
            .map(|p| self.path_policy.require_prompt_for_path(p))
            .unwrap_or(false)
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if let Some(error) = self.path_policy.deny_path(&args.path) {
            return json!({ "error": error });
        }
        let resolved = self.path_policy.resolve(&args.path);
        let path = resolved.normalized.display().to_string();
        if let Some(ext) = image_ext(&path) {
            return match tokio::fs::read(&resolved.normalized).await {
                Ok(bytes) => json!({
                    "type": "image",
                    "mimeType": format!("image/{ext}"),
                    "data": b64_encode(&bytes),
                }),
                Err(e) => map_err(&path, e),
            };
        }
        let content = match tokio::fs::read_to_string(&resolved.normalized).await {
            Ok(c) => c,
            Err(e) => return map_err(&path, e),
        };
        let lines: Vec<&str> = content.split('\n').collect();
        let total = lines.len();
        if let Some(o) = args.offset {
            if o > total {
                return json!({
                    "error": format!("offset {o} is past end of file ({total} lines)")
                });
            }
        }
        let start = args
            .offset
            .map(|o| o.saturating_sub(1))
            .unwrap_or(0)
            .min(total);
        let end = args.limit.map(|l| start + l).unwrap_or(total).min(total);
        let slice = &lines[start..end];
        let mut obj = serde_json::Map::new();
        obj.insert("content".into(), json!(slice.join("\n")));
        obj.insert("totalLines".into(), json!(total));
        if end < total {
            obj.insert("truncated".into(), json!(true));
            obj.insert("nextOffset".into(), json!(end + 1));
        }
        Value::Object(obj)
    }
}

fn map_err(path: &str, e: std::io::Error) -> Value {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => json!({ "error": format!("File not found: {path}") }),
        PermissionDenied => json!({ "error": format!("Permission denied: {path}") }),
        _ => json!({ "error": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_rfc4648_vectors() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn image_ext_detection() {
        assert_eq!(image_ext("a.png"), Some("png"));
        assert_eq!(image_ext("a.PNG"), Some("png"));
        assert_eq!(image_ext("a.jpg"), Some("jpeg"));
        assert_eq!(image_ext("a.jpeg"), Some("jpeg"));
        assert_eq!(image_ext("a.gif"), Some("gif"));
        assert_eq!(image_ext("a.webp"), Some("webp"));
        assert_eq!(image_ext("a.txt"), None);
        assert_eq!(image_ext("foo"), None);
    }

    #[tokio::test]
    async fn reads_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, "line1\nline2\nline3")
            .await
            .unwrap();

        let result = FileReadTool {
            path_policy: PathPolicy::default(),
        }
        .execute(json!({ "path": path.to_str().unwrap() }))
        .await;

        assert_eq!(result["content"].as_str().unwrap(), "line1\nline2\nline3");
        assert_eq!(result["totalLines"].as_u64().unwrap(), 3);
        assert!(result.get("truncated").is_none());
    }

    #[tokio::test]
    async fn reads_with_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "1\n2\n3\n4\n5").await.unwrap();

        let result = FileReadTool {
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "offset": 2,
            "limit": 2
        }))
        .await;

        assert_eq!(result["content"].as_str().unwrap(), "2\n3");
        assert!(result["truncated"].as_bool().unwrap());
        assert_eq!(result["nextOffset"].as_u64().unwrap(), 4);
    }

    #[tokio::test]
    async fn reads_with_offset_past_end_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "line1\nline2\nline3")
            .await
            .unwrap();

        let result = FileReadTool {
            path_policy: PathPolicy::default(),
        }
        .execute(json!({
            "path": path.to_str().unwrap(),
            "offset": 100
        }))
        .await;

        let err = result["error"].as_str().unwrap();
        assert!(err.contains("past end of file"), "{err}");
        assert!(err.contains("3 lines"), "{err}");
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let result = FileReadTool {
            path_policy: PathPolicy::default(),
        }
        .execute(json!({ "path": "/nonexistent/path/abc.xyz" }))
        .await;
        assert!(result["error"].as_str().unwrap().contains("not found"));
    }
}
