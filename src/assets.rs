//! The compiled frontend is embedded into the binary, so a deployment is a single file.
//!
//! Assets are compressed once (brotli and gzip, on first request) and served from memory
//! afterwards, so page loads cost no compression CPU no matter how many users there are.

use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex, OnceLock},
};

/// Content-Security-Policy for the single-page app: only our own scripts, no plugins, and no
/// framing by other sites. Inline styles are needed by CodeMirror.
const APP_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; worker-src 'self' blob:; \
    frame-src 'self'; frame-ancestors 'self'; base-uri 'self'; form-action 'self'";

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// One asset in its raw and compressed forms (compressed forms only when they are smaller).
struct Encoded {
    raw: Bytes,
    br: Option<Bytes>,
    gzip: Option<Bytes>,
}

fn compressible(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("javascript")
        || mime.contains("json")
        || mime.contains("svg")
        || mime.contains("wasm")
}

fn encode(raw: Bytes, mime: &str) -> Encoded {
    if !compressible(mime) || raw.len() < 512 {
        return Encoded {
            raw,
            br: None,
            gzip: None,
        };
    }
    let mut br = Vec::new();
    {
        let mut w = brotli::CompressorWriter::new(&mut br, 64 * 1024, 11, 22);
        let _ = w.write_all(&raw);
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = gz.write_all(&raw);
    let gzip = gz.finish().unwrap_or_default();
    let smaller = |v: Vec<u8>| (!v.is_empty() && v.len() < raw.len()).then(|| Bytes::from(v));
    Encoded {
        br: smaller(br),
        gzip: smaller(gzip),
        raw,
    }
}

type Cache = Mutex<HashMap<String, Arc<Encoded>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn lookup(name: &str) -> Option<Arc<Encoded>> {
    cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(name)
        .cloned()
}

/// Compresses and caches an asset. CPU-heavy: call from a blocking thread.
fn load(name: &str, mime: &str) -> Option<Arc<Encoded>> {
    if let Some(e) = lookup(name) {
        return Some(e);
    }
    let file = Assets::get(name)?;
    let e = Arc::new(encode(Bytes::from(file.data.into_owned()), mime));
    cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(name.to_string(), e.clone());
    Some(e)
}

/// Compresses every embedded asset in the background, so no user waits for it.
pub fn warm() {
    tokio::task::spawn_blocking(|| {
        for name in Assets::iter() {
            let mime = mime_guess::from_path(name.as_ref()).first_or_octet_stream();
            load(&name, mime.as_ref());
        }
        tracing::debug!("frontend assets compressed");
    });
}

fn accepts(headers: &HeaderMap, enc: &str) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|part| {
                let mut it = part.trim().split(';');
                let name = it.next().unwrap_or("").trim();
                let q0 = it.any(|p| p.trim().replace(' ', "") == "q=0");
                name.eq_ignore_ascii_case(enc) && !q0
            })
        })
}

pub async fn static_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path.starts_with("git/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let name = if !path.is_empty() && Assets::get(path).is_some() {
        path
    } else {
        "index.html"
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let asset = match lookup(name) {
        Some(a) => Some(a),
        None => {
            let (n, m) = (name.to_string(), mime.to_string());
            tokio::task::spawn_blocking(move || load(&n, &m))
                .await
                .ok()
                .flatten()
        }
    };
    let Some(asset) = asset else {
        return (StatusCode::NOT_FOUND, "frontend not built").into_response();
    };
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let (body, encoding) = match (&asset.br, &asset.gzip) {
        (Some(b), _) if accepts(&headers, "br") => (b.clone(), Some("br")),
        (_, Some(g)) if accepts(&headers, "gzip") => (g.clone(), Some("gzip")),
        _ => (asset.raw.clone(), None),
    };
    let mut res = (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response();
    let h = res.headers_mut();
    if let Some(enc) = encoding {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
    }
    if asset.br.is_some() || asset.gzip.is_some() {
        h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if name.ends_with(".html") {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(APP_CSP),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_encoding_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br;q=0"),
        );
        assert!(accepts(&h, "gzip"));
        assert!(!accepts(&h, "br"));
        h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("br"));
        assert!(accepts(&h, "br"));
    }

    #[test]
    fn compresses_text_only() {
        let text = Bytes::from("hello world ".repeat(200));
        let e = encode(text.clone(), "application/javascript");
        assert!(e.br.as_ref().unwrap().len() < text.len());
        assert!(e.gzip.is_some());
        assert!(encode(text, "image/png").br.is_none());
    }
}
