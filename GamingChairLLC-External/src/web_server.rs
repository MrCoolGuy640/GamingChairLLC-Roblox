//! Tiny local HTTP server that serves `resources/` to the WebView.
//!
//! Bound synchronously so a port problem is known before the window is created;
//! if the preferred port is taken, an OS-assigned free port is used instead.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use tiny_http::{Header, Request, Response, Server};

use crate::config::{self, Paths};

/// Always re-fetch from disk. The UI is served from memory-speed loopback, and
/// a persistent WebView profile would otherwise keep serving an old script or
/// stylesheet after the app has been updated.
const CACHE_CONTROL: &str = "no-store";

pub struct ResourceServer {
    server: Arc<Server>,
    base_url: String,
    handle: Option<JoinHandle<()>>,
}

impl ResourceServer {
    pub fn start(paths: &Paths) -> Result<Self> {
        let preferred = format!("{}:{}", config::HOST, config::PORT);
        let server = match Server::http(preferred.as_str()) {
            Ok(server) => server,
            Err(err) => {
                log::warn!(
                    target: "GamingChairLLC.http",
                    "Port {} unavailable ({err}); using a free port instead",
                    config::PORT
                );
                Server::http(format!("{}:0", config::HOST).as_str()).map_err(|err| {
                    anyhow::anyhow!("binding the resource server: {err}")
                })?
            }
        };

        let port = server
            .server_addr()
            .to_ip()
            .map(|address| address.port())
            .unwrap_or(0);
        let base_url = format!("http://{}:{port}", config::HOST);

        let server = Arc::new(server);
        let thread_server = Arc::clone(&server);
        let root = paths.resources.clone();

        let handle = thread::Builder::new()
            .name("ResourceServer".into())
            .spawn(move || serve_loop(&thread_server, &root))
            .context("starting the resource server thread")?;

        log::info!(
            target: "GamingChairLLC.http",
            "Resource server listening on {base_url}"
        );

        Ok(Self {
            server,
            base_url,
            handle: Some(handle),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for ResourceServer {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_loop(server: &Server, root: &Path) {
    for request in server.incoming_requests() {
        serve(root, request);
    }
}

fn serve(root: &Path, request: Request) {
    let target = normalize_target(request.url());

    if target.split('/').any(|part| part == "..") {
        log::warn!(
            target: "GamingChairLLC.http",
            "Rejected path traversal request: {}",
            request.url()
        );
        respond_text(request, 403, "403 Forbidden");
        return;
    }

    let relative = if target.is_empty() || target == "/" {
        "web/index.html"
    } else {
        target.trim_start_matches('/')
    };

    let mut path = PathBuf::from(root);
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        path.push(part);
    }

    match fs::read(&path) {
        Ok(bytes) => {
            log::debug!(
                target: "GamingChairLLC.http",
                "200 {} ({} bytes)",
                request.url(),
                bytes.len()
            );

            let response = Response::from_data(bytes)
                .with_header(header("Content-Type", mime_for(&path)))
                .with_header(header("Cache-Control", CACHE_CONTROL));
            let _ = request.respond(response);
        }
        Err(_) => {
            log::warn!(
                target: "GamingChairLLC.http",
                "404 Not Found: {}",
                request.url()
            );
            respond_text(request, 404, "404 Not Found");
        }
    }
}

fn respond_text(request: Request, status: u16, body: &str) {
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "text/plain; charset=utf-8"));
    let _ = request.respond(response);
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static HTTP headers are valid ASCII")
}

/// Strip a query string and percent-decode the path.
fn normalize_target(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);

    // Some clients send an absolute-form request target.
    let path = if let Some(rest) = path.strip_prefix("http://") {
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else if let Some(rest) = path.strip_prefix("https://") {
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else {
        path
    };

    percent_decode(path)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                out.push(high << 4 | low);
                index += 3;
                continue;
            }
        }

        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn mime_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
