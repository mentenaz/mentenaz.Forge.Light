//! Client for the Forge extension registry (`/api/registry/extensions*`),
//! which mirrors the shape of Zed's `/extensions` API.

use crate::{Error, Provides, manifest::valid_id};
use serde::Deserialize;
use std::time::Duration;

pub const DEFAULT_BASE_URL: &str = "https://forge.mentenaz-server.com/api/registry";

/// Manifest schema versions this build can read.
pub const SCHEMA_VERSIONS: (u32, u32) = (0, 1);
/// Wasm extension API versions, as `(min, max)`. Forge loads no wasm yet, so
/// this only keeps the registry from handing out archives built for newer APIs.
pub const WASM_API_VERSIONS: (&str, &str) = ("0.0.1", "0.8.0");

const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

/// One extension as the registry lists it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub wasm_api_version: Option<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub download_count: u64,
}

#[derive(Deserialize)]
struct Listing {
    data: Vec<Entry>,
}

pub struct Registry {
    base: String,
    agent: ureq::Agent,
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new(DEFAULT_BASE_URL)
    }
}

impl Registry {
    pub fn new(base: impl Into<String>) -> Self {
        // Downloads are redirected to the extension's release asset, so redirects are followed.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(5)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Registry { base: base.into().trim_end_matches('/').to_owned(), agent }
    }

    /// Extensions matching `search` (name or description) and offering any of `provides`.
    pub fn list(&self, search: Option<&str>, provides: &[Provides]) -> Result<Vec<Entry>, Error> {
        let mut query = vec![("max_schema_version", SCHEMA_VERSIONS.1.to_string())];
        if let Some(search) = search.filter(|s| !s.is_empty()) {
            query.push(("filter", search.to_owned()));
        }
        if !provides.is_empty() {
            query.push(("provides", provides.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(",")));
        }
        self.listing("/extensions", &query)
    }

    /// The entries among `ids` that have a newer version than the registry's latest compatible one.
    pub fn latest(&self, ids: &[&str]) -> Result<Vec<Entry>, Error> {
        if let Some(bad) = ids.iter().find(|id| !valid_id(id)) {
            return Err(Error::InvalidId((*bad).to_owned()));
        }
        let query = vec![
            ("min_schema_version", SCHEMA_VERSIONS.0.to_string()),
            ("max_schema_version", SCHEMA_VERSIONS.1.to_string()),
            ("min_wasm_api_version", WASM_API_VERSIONS.0.to_owned()),
            ("max_wasm_api_version", WASM_API_VERSIONS.1.to_owned()),
            ("ids", ids.join(",")),
        ];
        self.listing("/extensions/updates", &query)
    }

    /// The archive (`.tar.gz`) of an extension: its latest compatible version, or `version`.
    pub fn download(&self, id: &str, version: Option<&str>) -> Result<Vec<u8>, Error> {
        if !valid_id(id) {
            return Err(Error::InvalidId(id.to_owned()));
        }
        let (path, query) = match version {
            Some(v) if valid_id(v) => (format!("/extensions/{id}/{v}/download"), Vec::new()),
            Some(v) => return Err(Error::InvalidId(v.to_owned())),
            None => (
                format!("/extensions/{id}/download"),
                vec![
                    ("min_schema_version", SCHEMA_VERSIONS.0.to_string()),
                    ("max_schema_version", SCHEMA_VERSIONS.1.to_string()),
                    ("min_wasm_api_version", WASM_API_VERSIONS.0.to_owned()),
                    ("max_wasm_api_version", WASM_API_VERSIONS.1.to_owned()),
                ],
            ),
        };
        let mut response = self.get(&path, &query)?;
        let status = response.status().as_u16();
        match status {
            200 => {}
            404 => return Err(Error::NotFound(id.to_owned())),
            _ => return Err(Error::Registry(format!("answered {status} when downloading {id:?}"))),
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|e| Error::Registry(format!("reading {id:?}: {e}")))
    }

    fn listing(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<Entry>, Error> {
        let mut response = self.get(path, query)?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().map_err(|e| Error::Registry(format!("reading the listing: {e}")))?;
        if status != 200 {
            let note: String = body.chars().take(200).collect();
            return Err(Error::Registry(format!("answered {status}: {note}")));
        }
        let listing: Listing = serde_json::from_str(&body).map_err(|e| Error::Registry(format!("unreadable listing: {e}")))?;
        Ok(listing.data)
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<ureq::http::Response<ureq::Body>, Error> {
        let mut request = self.agent.get(format!("{}{path}", self.base));
        for (k, v) in query {
            request = request.query(k, v);
        }
        request.call().map_err(|e| Error::Registry(format!("{}{path}: {e}", self.base)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serves canned responses, one connection each, and records the request lines.
    fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or_default().to_owned());
                stream.write_all(response.as_bytes()).unwrap();
            }
            seen
        });
        (base, handle)
    }

    fn ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[test]
    fn a_listing_is_parsed_and_the_filters_are_sent() {
        let body = r#"{"data":[{"id":"zord-theme","name":"Zord Theme","version":"1.0.0","authors":["A"],"schema_version":1,"wasm_api_version":null,"provides":["themes"],"download_count":0}]}"#;
        let (base, server) = serve(vec![ok(body)]);
        let found = Registry::new(base).list(Some("zord"), &[Provides::Themes, Provides::IconThemes]).unwrap();
        assert_eq!((found[0].id.as_str(), found[0].provides.as_slice()), ("zord-theme", ["themes".to_owned()].as_slice()));
        let request = &server.join().unwrap()[0];
        assert!(request.starts_with("GET /extensions?"), "{request}");
        assert!(request.contains("filter=zord") && request.contains("provides=themes%2Cicon-themes"), "{request}");
    }

    #[test]
    fn a_download_follows_the_redirect_to_the_release_asset() {
        let (base, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /asset.tar.gz\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".into(),
        ]);
        assert_eq!(Registry::new(base).download("zord-theme", None).unwrap(), b"hello");
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("GET /extensions/zord-theme/download?"), "{seen:?}");
        assert!(seen[1].starts_with("GET /asset.tar.gz"), "{seen:?}");
    }

    #[test]
    fn a_missing_extension_and_a_server_error_are_told_apart() {
        let (base, _s) = serve(vec!["HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        assert!(matches!(Registry::new(base).download("nope", None), Err(Error::NotFound(id)) if id == "nope"));
        let (base, _s) = serve(vec!["HTTP/1.1 500 Oops\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        assert!(matches!(Registry::new(base).download("x", None), Err(Error::Registry(m)) if m.contains("500")));
    }

    #[test]
    fn ids_that_could_change_the_url_are_refused_before_any_request() {
        let registry = Registry::new("http://127.0.0.1:1");
        for bad in ["../x", "a/b", "a?b", ""] {
            assert!(matches!(registry.download(bad, None), Err(Error::InvalidId(_))), "{bad:?}");
        }
        assert!(matches!(registry.download("ok", Some("1/2")), Err(Error::InvalidId(_))));
        assert!(matches!(registry.latest(&["a", "b/c"]), Err(Error::InvalidId(_))));
    }

    /// Talks to the real registry: `cargo test -p forge_extensions -- --ignored`.
    #[test]
    #[ignore]
    fn the_live_registry_lists_themes() {
        let found = Registry::default().list(None, &[Provides::Themes]).unwrap();
        assert!(!found.is_empty());
        assert!(found.iter().all(|e| e.provides.iter().any(|p| p == "themes")));
    }
}
