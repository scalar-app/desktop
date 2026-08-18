//! Scalar's native shell.
//!
//! The interface is the same web app that runs in a browser, loaded from files inside the bundle.
//! This crate exists for one thing the webview cannot do: carry the session.
//!
//! A packaged app is served from `tauri://localhost`, so every call to a Scalar server is cross
//! origin. The session cookie is `HttpOnly` and `SameSite=Lax`, which means a webview will not send
//! it, and the API's CORS allowlist would not name this origin anyway. Rather than loosening either
//! of those on the server, requests are made here, where a normal HTTP client with its own cookie
//! jar applies. Nothing about the API changes to support native apps.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// One request from the web app, mirroring the shape of `fetch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    /// Always text: the SDK sends JSON and nothing else.
    body: Option<String>,
}

/// Enough of a `Response` for the web app to rebuild one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse {
    status: u16,
    status_text: String,
    headers: HashMap<String, String>,
    body: String,
}

/// The HTTP client, holding the cookie jar for the life of the app.
pub struct Http(reqwest::Client);

fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .cookie_store(true)
        .user_agent(concat!("Scalar/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("could not start the HTTP client: {error}"))
}

/// Performs one API request outside the webview, so the session cookie travels with it.
///
/// Only the transport lives here. Which server to talk to, what to send and what to do with the
/// answer are all decided by the web app, exactly as they are in a browser.
#[tauri::command]
async fn api_fetch(
    state: tauri::State<'_, Arc<Http>>,
    request: ApiRequest,
) -> Result<ApiResponse, String> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| format!("unsupported method: {}", request.method))?;

    let mut outgoing = state.0.request(method, &request.url);
    for (name, value) in &request.headers {
        outgoing = outgoing.header(name, value);
    }
    if let Some(body) = request.body {
        outgoing = outgoing.body(body);
    }

    // A failure to reach the server is reported as an error string, which the web app turns into
    // the same "could not connect" state a browser would show.
    let response = outgoing
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;

    let status = response.status();
    let mut headers = HashMap::new();
    for (name, value) in response.headers() {
        if let Ok(text) = value.to_str() {
            headers.insert(name.as_str().to_ascii_lowercase(), text.to_string());
        }
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("could not read the response: {error}"))?;

    Ok(ApiResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
    })
}

/// Runs before any page script, so the app finds its transport already in place.
///
/// It defines the hook `lib/api.ts` looks for. The web app stays unaware that it is running in a
/// native shell: it asks for a fetch and gets one.
const BOOTSTRAP: &str = r#"
(() => {
  const invoke = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke;
  if (!invoke) return;

  window.__SCALAR_NATIVE__ = true;

  window.__SCALAR_FETCH__ = async (url, init = {}) => {
    const headers = {};
    new Headers(init.headers || {}).forEach((value, key) => {
      headers[key] = value;
    });

    const result = await invoke('api_fetch', {
      request: {
        url,
        method: (init.method || 'GET').toUpperCase(),
        headers,
        body: typeof init.body === 'string' ? init.body : null,
      },
    });

    return new Response(result.body === '' ? null : result.body, {
      status: result.status,
      statusText: result.statusText,
      headers: result.headers,
    });
  };
})();
"#;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(Arc::new(Http(build_client()?)));

            // Built here rather than declared in the config, because an initialization script has
            // to be attached to the window and it has to run before the app's own scripts.
            let mut window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Scalar")
                .initialization_script(BOOTSTRAP);

            // Sizing is meaningless on a phone, where the window is the screen.
            #[cfg(desktop)]
            {
                window = window
                    .inner_size(1100.0, 760.0)
                    .min_inner_size(380.0, 560.0)
                    .resizable(true);
            }

            window.build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![api_fetch])
        .run(tauri::generate_context!())
        .expect("error while running Scalar");
}
