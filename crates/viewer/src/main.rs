//! Local web viewer: `grim-viewer [--port N] [game_dir]`, then open http://127.0.0.1:N/.

mod api;
mod decode;

use std::collections::HashMap;
use std::path::PathBuf;

use tiny_http::{Header, Response, Server};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(v: serde_json::Value) -> Self {
        Self { status: 200, content_type: "application/json", body: v.to_string().into_bytes() }
    }

    pub fn bytes(content_type: &'static str, body: Vec<u8>) -> Self {
        Self { status: 200, content_type, body }
    }

    pub fn error(status: u16, msg: impl Into<String>) -> Self {
        Self { status, content_type: "text/plain; charset=utf-8", body: msg.into().into_bytes() }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut port = 8765u16;
    let mut root: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()).expect("--port <number>"),
            other => root = Some(PathBuf::from(other)),
        }
    }
    let root = root.unwrap_or_else(grim_testkit::require_game_dir);
    let state = api::State::new(root);
    let server = Server::http(("127.0.0.1", port)).expect("cannot bind the viewer port");
    println!("grim-viewer: http://127.0.0.1:{port}/");

    for req in server.incoming_requests() {
        let url = req.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let q = parse_query(query);
        let reply = match path {
            "/" => Reply::bytes("text/html; charset=utf-8", INDEX_HTML.as_bytes().to_vec()),
            "/app.js" => Reply::bytes("text/javascript; charset=utf-8", APP_JS.as_bytes().to_vec()),
            p if p.starts_with("/api/") => {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.handle(&p[5..], &q)));
                r.unwrap_or_else(|_| Reply::error(500, "handler panicked"))
            }
            _ => Reply::error(404, "not found"),
        };
        let header = Header::from_bytes("Content-Type", reply.content_type).unwrap();
        let resp = Response::from_data(reply.body).with_status_code(reply.status).with_header(header);
        let _ = req.respond(resp);
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
