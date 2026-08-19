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
//!
//! Everything else here exists because a webview on its own is not an application: it has no menu,
//! so on macOS copy and paste do not work, it has no zoom, and a link to another site would
//! navigate the window away from the app with no way back.

use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest_cookie_store::CookieStoreMutex;
use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// How long to wait on a Scalar server before giving up.
///
/// Without this a server that accepts the connection and then says nothing leaves the app waiting
/// for ever, with no way for the person to tell that anything is wrong.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One request from the web app, mirroring the shape of `fetch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    /// Bytes, so an upload is not corrupted on the way through.
    body: Option<Vec<u8>>,
}

/// Enough of a `Response` for the web app to rebuild one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse {
    status: u16,
    status_text: String,
    headers: HashMap<String, String>,
    /// Bytes rather than text: attachments, images and avatars are not valid UTF-8, and decoding
    /// them as a string replaces every byte it does not understand.
    body: Vec<u8>,
}

/// The name of the jar on disk. Kept in the app data directory, beside the window geometry.
const COOKIE_FILE: &str = "cookies.json";

/// The HTTP client and the cookie jar it draws on, for the life of the app.
///
/// The jar is written to disk because the alternative is signing in again every single launch.
/// The window remembers its size and the app remembers its server, so a session that did not
/// survive closing the window was the one thing that made the app feel disposable.
///
/// What that means in practice: the session cookie sits in a file in your own user profile,
/// readable by anything running as you. That is the same bargain every browser makes with its
/// cookie database, and the file holds one cookie for one self-hosted server. Deleting it, or
/// signing out, is enough to end the session.
pub struct Http {
    client: reqwest::Client,
    jar: Arc<CookieStoreMutex>,
    path: PathBuf,
}

impl Http {
    /// Writes the jar out. Called after a response that changed it, and again on the way out.
    ///
    /// Failure is deliberately quiet: not being able to save a cookie is a reason to sign in
    /// again next time, not a reason to interrupt somebody mid task. Nothing is logged, because
    /// the thing that failed to write is a credential.
    fn persist(&self) {
        let Ok(store) = self.jar.lock() else { return };
        let Some(parent) = self.path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let mut buffer = Vec::new();
        if cookie_store::serde::json::save(&store, &mut buffer).is_err() {
            return;
        }
        let _ = fs::write(&self.path, buffer);
    }
}

/// Reads the jar back, or starts an empty one.
///
/// A file that will not parse is treated as no file at all. A corrupt jar should cost somebody a
/// fresh sign in, never a start up failure.
fn load_jar(path: &Path) -> cookie_store::CookieStore {
    let Ok(file) = fs::File::open(path) else {
        return cookie_store::CookieStore::default();
    };
    cookie_store::serde::json::load(BufReader::new(file)).unwrap_or_default()
}

fn build_client(jar: Arc<CookieStoreMutex>) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .cookie_provider(jar)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
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

    let mut outgoing = state.client.request(method, &request.url);
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
    // Signing in and signing out are the only responses that change the jar, so this writes
    // rarely rather than on every request.
    if headers.contains_key("set-cookie") {
        state.persist();
    }

    let body = response
        .bytes()
        .await
        .map_err(|error| format!("could not read the response: {error}"))?
        .to_vec();

    Ok(ApiResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
    })
}

/// Runs before any page script, so the app finds its transport already in place.
///
/// Note what this does not do: it does not make remote images render. The content security
/// policy keeps `img-src` to `'self' data:` on purpose, because a remote image in an email is
/// usually a tracking pixel. Carrying image bytes correctly and choosing to display them are
/// separate decisions, and only the first one belongs here. See the README.
///
/// It defines the hook `lib/api.ts` looks for. The web app stays unaware that it is running in a
/// native shell: it asks for a fetch and gets one.
///
/// Bodies cross the boundary as arrays of bytes in both directions, because JSON has no way to
/// carry arbitrary binary and anything lossy here would corrupt an attachment.
const BOOTSTRAP: &str = r#"
(() => {
  const invoke = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke;
  if (!invoke) return;

  window.__SCALAR_NATIVE__ = true;

  const toBytes = async (body) => {
    if (body == null) return null;
    if (typeof body === 'string') return Array.from(new TextEncoder().encode(body));
    if (body instanceof ArrayBuffer) return Array.from(new Uint8Array(body));
    if (ArrayBuffer.isView(body)) {
      return Array.from(new Uint8Array(body.buffer, body.byteOffset, body.byteLength));
    }
    if (body instanceof Blob) return Array.from(new Uint8Array(await body.arrayBuffer()));
    return Array.from(new TextEncoder().encode(String(body)));
  };

  window.__SCALAR_FETCH__ = async (url, init = {}) => {
    const headers = {};
    new Headers(init.headers || {}).forEach((value, key) => {
      headers[key] = value;
    });

    let result;
    try {
      result = await invoke('api_fetch', {
        request: {
          url,
          method: (init.method || 'GET').toUpperCase(),
          headers,
          body: await toBytes(init.body),
        },
      });
    } catch (reason) {
      // Tauri rejects a failed command with a plain string. The SDK keeps the message only when
      // the cause is an Error, so without this every transport failure reads as a generic
      // "network request failed" and the real reason, such as a timeout or a refused
      // connection, is thrown away. TypeError is what fetch itself throws when a request never
      // completes, so the web app handles this exactly as it handles a browser failure.
      throw new TypeError(typeof reason === 'string' ? reason : 'Network request failed');
    }

    const bytes = new Uint8Array(result.body);
    // 204 and 304 must be constructed with a null body or the Response constructor throws.
    const empty = bytes.byteLength === 0 || result.status === 204 || result.status === 304;
    return new Response(empty ? null : bytes, {
      status: result.status,
      statusText: result.statusText,
      headers: result.headers,
    });
  };
})();
"#;

/// Whether a navigation is the app itself rather than somebody's link.
///
/// The bundled interface is served from `tauri://localhost`, and on Windows from
/// `http://tauri.localhost`. Following anything else in this window would replace the app with a
/// web page and leave no way back, because there is no address bar and no back button.
fn is_internal(url: &tauri::Url) -> bool {
    match url.scheme() {
        // The custom protocols the bundle is served over.
        "tauri" | "asset" => true,
        // Only the loopback names Tauri itself serves on, never a real site.
        "http" | "https" => matches!(url.host_str(), Some("localhost" | "tauri.localhost")),
        _ => false,
    }
}

/// The menu.
///
/// A webview provides no editing commands of its own. On macOS that is not a rough edge but a
/// blocker: without an Edit menu carrying the standard roles, the system shortcuts for copy, paste
/// and select all do nothing at all. Zoom is here for the same reason.
#[cfg(desktop)]
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{AboutMetadata, MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    // Equal rather than Plus: the plus character needs Shift on most layouts, and Tauri does not
    // parse "Plus" as a key at all, which silently left Zoom In with no shortcut. Ctrl+= is what
    // browsers bind, and it is reached without a modifier dance.
    let zoom_in = MenuItemBuilder::with_id("zoom-in", "Zoom In")
        .accelerator("CmdOrCtrl+=")
        .build(app)?;
    let zoom_out = MenuItemBuilder::with_id("zoom-out", "Zoom Out")
        .accelerator("CmdOrCtrl+-")
        .build(app)?;
    let zoom_reset = MenuItemBuilder::with_id("zoom-reset", "Actual Size")
        .accelerator("CmdOrCtrl+0")
        .build(app)?;

    let app_menu = SubmenuBuilder::new(app, "Scalar")
        .about(Some(AboutMetadata {
            name: Some("Scalar".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            ..Default::default()
        }))
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "View")
        .item(&zoom_in)
        .item(&zoom_out)
        .item(&zoom_reset)
        .separator()
        .fullscreen()
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;

    MenuBuilder::new(app)
        .items(&[&app_menu, &edit_menu, &view_menu, &window_menu])
        .build()
}

/// Steps the webview zoom, clamped so it cannot be driven to something unreadable.
#[cfg(desktop)]
fn apply_zoom(window: &tauri::WebviewWindow, change: f64, reset: bool) {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Tauri has no getter for the current zoom, so it is tracked here. Stored as bits because
    // there is no atomic float.
    static ZOOM: AtomicU64 = AtomicU64::new(0);
    let current = match ZOOM.load(Ordering::Relaxed) {
        0 => 1.0,
        bits => f64::from_bits(bits),
    };

    let next = if reset {
        1.0
    } else {
        (current + change).clamp(0.5, 2.0)
    };
    ZOOM.store(next.to_bits(), Ordering::Relaxed);
    let _ = window.set_zoom(next);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    // Both of these are meaningless on a phone, where the system owns the window and only ever
    // runs one copy of an app.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Launching again should bring the window you already have to the front rather than
            // opening a second one with the same session.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));

    builder
        .setup(|app| {
            // Beside the window geometry, in the directory the platform gives this app for its
            // own data rather than anywhere shared.
            let path = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("no app data directory: {error}"))?
                .join(COOKIE_FILE);
            let jar = Arc::new(CookieStoreMutex::new(load_jar(&path)));
            app.manage(Arc::new(Http {
                client: build_client(jar.clone())?,
                jar,
                path,
            }));

            #[cfg(desktop)]
            {
                let menu = build_menu(app.handle())?;
                app.set_menu(menu)?;
                app.on_menu_event(|app, event| {
                    let Some(window) = app.get_webview_window("main") else {
                        return;
                    };
                    match event.id().as_ref() {
                        "zoom-in" => apply_zoom(&window, 0.1, false),
                        "zoom-out" => apply_zoom(&window, -0.1, false),
                        "zoom-reset" => apply_zoom(&window, 0.0, true),
                        _ => {}
                    }
                });
            }

            // Built here rather than declared in the config, because an initialization script has
            // to be attached to the window and it has to run before the app's own scripts.
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Scalar")
                .initialization_script(BOOTSTRAP)
                .on_navigation(|url| {
                    if is_internal(url) {
                        return true;
                    }
                    // Somebody's link. Hand it to their browser, where it has an address bar and a
                    // back button, and leave this window on the app.
                    if matches!(url.scheme(), "http" | "https" | "mailto") {
                        let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
                    }
                    false
                });

            // Sizing is meaningless on a phone, where the window is the screen. The plugin
            // restores the previous geometry over these defaults when there is one.
            #[cfg(desktop)]
            let window = window
                .inner_size(1100.0, 760.0)
                .min_inner_size(380.0, 560.0)
                .resizable(true);

            let window = window.build()?;

            // A window created here rather than declared in the config is not restored by the
            // plugin on its own, so the saved geometry is applied explicitly.
            #[cfg(desktop)]
            {
                use tauri_plugin_window_state::{StateFlags, WindowExt};
                let _ = window.restore_state(StateFlags::all());
            }
            let _ = &window;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![api_fetch])
        .build(tauri::generate_context!())
        .expect("error while building Scalar")
        .run(|app, event| {
            // Signing in already saves the jar. This covers the rest: cookies the server rotated
            // or expired during the session, which would otherwise be lost on the way out.
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(http) = app.try_state::<Arc<Http>>() {
                    http.persist();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{build_client, is_internal, load_jar, Http, CONNECT_TIMEOUT};
    use reqwest_cookie_store::CookieStoreMutex;
    use std::sync::Arc;
    use tauri::Url;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("test url should parse")
    }

    #[test]
    fn keeps_the_bundled_app_in_the_window() {
        assert!(is_internal(&url("tauri://localhost/today")));
        assert!(is_internal(&url("http://tauri.localhost/ask")));
        assert!(is_internal(&url("asset://localhost/icon.png")));
    }

    #[test]
    fn sends_real_sites_to_the_browser() {
        assert!(!is_internal(&url("https://github.com/scalar-app")));
        assert!(!is_internal(&url("https://mail.google.com/mail/u/0")));
        assert!(!is_internal(&url("mailto:someone@example.com")));
    }

    /// A host that merely ends in the trusted name is a different host, and treating it as
    /// internal would let a link navigate the app away to a site somebody else controls.
    #[test]
    fn does_not_confuse_a_lookalike_host() {
        assert!(!is_internal(&url("https://tauri.localhost.example.com/")));
        assert!(!is_internal(&url("https://notlocalhost/")));
        assert!(!is_internal(&url("https://evil.com/?x=tauri.localhost")));
    }

    /// A server that is not there has to fail, and fail quickly, rather than leaving the
    /// request outstanding with nothing on screen to explain it. The error is what the command
    /// turns into a string, which the shim rethrows so the web app shows its usual failure.
    #[tokio::test]
    async fn a_server_that_is_not_there_fails_rather_than_hanging() {
        let jar = Arc::new(CookieStoreMutex::new(cookie_store::CookieStore::default()));
        let client = build_client(jar).expect("client should build");

        let started = std::time::Instant::now();
        // Port 1 on loopback: nothing listens there, so the connection is refused outright.
        let result = client.get("http://127.0.0.1:1/health").send().await;

        assert!(
            result.is_err(),
            "a refused connection must surface as an error"
        );
        assert!(
            started.elapsed() < CONNECT_TIMEOUT,
            "a refused connection should fail immediately, not wait out the connect timeout",
        );
    }

    /// The whole point of the jar: a session outlives the process that created it.
    #[test]
    fn a_session_survives_being_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("scalar-jar-{}", std::process::id()));
        let path = dir.join("cookies.json");
        let _ = std::fs::remove_dir_all(&dir);

        let url = Url::parse("http://localhost:4000/").expect("url should parse");
        let store = {
            let mut store = cookie_store::CookieStore::default();
            store
                .parse(
                    "scalar_session=abc123; Path=/; Expires=Wed, 01 Jan 2098 00:00:00 GMT",
                    &url,
                )
                .expect("cookie should parse");
            store
        };

        let http = Http {
            client: build_client(Arc::new(CookieStoreMutex::new(
                cookie_store::CookieStore::default(),
            )))
            .expect("client should build"),
            jar: Arc::new(CookieStoreMutex::new(store)),
            path: path.clone(),
        };
        http.persist();
        assert!(path.exists(), "the jar should have been written");

        let reloaded = load_jar(&path);
        let cookie = reloaded
            .get("localhost", "/", "scalar_session")
            .expect("the session cookie should come back");
        assert_eq!(cookie.value(), "abc123");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Proves the feature end to end against a real Scalar server: sign in with one client,
    /// write the jar, build a second client from what was written, and still be signed in.
    ///
    /// Ignored by default because it needs a server. Run it with a Scalar API on port 4000:
    ///     cargo test --lib -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a Scalar API running on http://localhost:4000"]
    async fn a_session_outlives_the_process_that_signed_in() {
        let dir = std::env::temp_dir().join(format!("scalar-live-jar-{}", std::process::id()));
        let path = dir.join("cookies.json");
        let _ = std::fs::remove_dir_all(&dir);

        let jar = Arc::new(CookieStoreMutex::new(cookie_store::CookieStore::default()));
        let first = Http {
            client: build_client(jar.clone()).expect("client should build"),
            jar,
            path: path.clone(),
        };

        // Development mode hands the link back, which is the only way in while email delivery
        // does not exist.
        let requested: serde_json::Value = first
            .client
            .post("http://localhost:4000/api/v1/auth/magic-link")
            .json(&serde_json::json!({ "email": "jar-test@example.com" }))
            .send()
            .await
            .expect("the API should answer")
            .json()
            .await
            .expect("the response should be JSON");
        // The link the API returns points at the browser app it is configured for, not at the
        // API itself. Only the token matters, which is the same reason the web app carries it to
        // its own verify route rather than following this URL.
        let link = requested["devLink"]
            .as_str()
            .expect("the API must be in development mode for this test");
        let (_, token) = link
            .split_once("token=")
            .expect("the link should carry a token");

        let verified = first
            .client
            .get(format!(
                "http://localhost:4000/api/v1/auth/magic-link/verify?token={token}"
            ))
            .send()
            .await
            .expect("verify should answer");
        assert_eq!(
            verified.status(),
            200,
            "the sign in link should be accepted"
        );

        first.persist();
        assert!(path.exists(), "signing in should have written the jar");

        // A brand new client, exactly as the next launch of the app builds one.
        let reloaded = Arc::new(CookieStoreMutex::new(load_jar(&path)));
        let second = build_client(reloaded).expect("client should build");
        let me = second
            .get("http://localhost:4000/api/v1/me")
            .send()
            .await
            .expect("me should answer");

        assert_eq!(
            me.status(),
            200,
            "the restored session should still be signed in"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A jar that will not parse must cost a fresh sign in, never a failure to start.
    #[test]
    fn a_corrupt_jar_is_treated_as_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("scalar-bad-jar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir should be creatable");
        let path = dir.join("cookies.json");
        std::fs::write(&path, b"this is not json").expect("file should be writable");

        assert_eq!(load_jar(&path).iter_any().count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
