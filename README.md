<div align="center">
  <img src="https://raw.githubusercontent.com/scalar-app/.github/main/profile/assets/scalar.png" alt="Scalar" width="96" />
  <h1>Scalar for desktop and mobile</h1>
  <p>macOS, Windows, Linux, iOS and Android, from one codebase.</p>
</div>

---

This is the native shell around Scalar. It is not a second interface: it loads the same web app, built as static files and bundled inside the app, so a change to a screen reaches every platform without being written twice.

Tauri 2 is the reason one repository can cover all five targets. The Rust core here builds as a binary on desktop and as a library on iOS and Android, and the interface is a system webview rather than a bundled browser, which is why the download is measured in megabytes rather than hundreds of them.

## What the native part actually does

Little, deliberately. One job it exists for, and a handful of things a webview does not provide.

A packaged app is served from `tauri://localhost`, so every call to a Scalar server is cross origin. The session cookie is `HttpOnly` and `SameSite=Lax`, which means a webview will not send it, and the API's CORS allowlist would not name that origin in any case. The alternatives were to weaken the cookie to `SameSite=None` or to widen CORS on every self-hosted server, both of which make the browser deployment less safe to fix something that is not the browser's problem.

Instead, requests are made from Rust, where an ordinary HTTP client with its own cookie jar applies. `src-tauri/src/lib.rs` exposes a single `api_fetch` command and injects a `window.__SCALAR_FETCH__` shim before the app boots. The web app asks the SDK for a custom fetch, which it already supports, and never learns it is running natively. **The API is unchanged.** Bodies cross that boundary as bytes in both directions, so an attachment or an avatar is not corrupted by being decoded as text, and the client has connect and request timeouts so an unresponsive server fails instead of hanging.

The rest is what a window needs to be an application rather than a page:

- **Links leave the app properly.** Anything that is not the bundled interface opens in the reader's own browser. Following it in this window would replace the app with a web page and leave no way back, because there is no address bar and no back button.
- **A menu.** A webview has no editing commands of its own, and on macOS that means copy, paste and select all genuinely do nothing without an Edit menu carrying the standard roles. Zoom in, out and actual size are there for the same reason.
- **The window is remembered.** Size and position are restored, rather than reopening at the same default size every launch.
- **One instance.** Launching again focuses the window you already have instead of opening a second copy on the same session.

## Remote content, and why the CSP stays narrow

The content security policy in `tauri.conf.json` sets `img-src 'self' data:`. Remote images do not load, and that is the intended behaviour rather than an oversight to fix later.

Scalar is built around email, and a remote image in an email is usually a tracking pixel. Loading one tells the sender that the message was opened, when, from which IP address, and with which client. Every mail client that takes its readers seriously blocks remote content by default and asks first, so widening `img-src` to allow arbitrary origins would quietly turn the desktop app into the least private way to read your mail.

Making the transport binary safe does not change this. `api_fetch` can now carry image bytes correctly, but the webview still refuses to render an `<img>` pointing at a remote origin, which is the layer doing the protecting.

When loading remote images becomes a feature, it should work like this, and none of it needs the CSP to allow remote origins:

1. The reader asks for images in a particular message, per sender or per message. It is never automatic.
2. The image is fetched through `api_fetch`, in Rust, so the request carries no cookies from the webview and can be logged, refused or routed.
3. The bytes are handed back and turned into a `blob:` URL, which the interface renders.
4. `blob:` is added to `img-src` at that point, and nothing else. A `blob:` URL refers to content this app already fetched and holds, not to a remote origin, so it does not reintroduce the leak.

That work belongs in the `web` repository, since it is interface behaviour. The only change here would be the single `blob:` addition, made when there is something to render rather than in advance.

## Which server it talks to

Scalar is self-hosted and there is no hosted service, so a packaged app has no default server to point at. On first run it asks for the address of the Scalar API you are running, remembers it, and gets on with it. That screen lives in the web app (`ServerSetup`), so it is the same on every platform, including a browser build that was put up without a baked in API URL.

## Requirements

- Node 24 and pnpm 11
- Rust stable
- Platform toolchain:
  - **Windows**: Microsoft C++ Build Tools and WebView2. Use the MSVC toolchain.

    The GNU toolchain fails to link with `export ordinal too large`, because mingw's `ld` cannot
    export the number of symbols a `cdylib` of this size produces. That crate type exists only for
    Android, so if MSVC is genuinely not an option, a desktop only build links after temporarily
    reducing `[lib] crate-type` in `src-tauri/Cargo.toml` to `["rlib"]`. Do not commit that: iOS
    needs `staticlib` and Android needs `cdylib`, and CI builds both.

  - **macOS**: Xcode command line tools
  - **Linux**: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf`
- Sibling checkouts of [`web`](https://github.com/scalar-app/web), [`ui`](https://github.com/scalar-app/ui) and [`sdk`](https://github.com/scalar-app/sdk), the same arrangement the `link:` dependencies use elsewhere. Set `SCALAR_WEB_DIR` if `web` is somewhere else.

## Run it

```bash
pnpm install
pnpm dev
```

`pnpm dev` builds the web app as static files, copies them to `dist/`, and opens the window. `pnpm build` produces an installer for the platform you are on.

## Mobile

```bash
pnpm ios:init      # once, needs Xcode
pnpm ios:dev

pnpm android:init  # once, needs Android Studio and the NDK
pnpm android:dev
```

These generate `gen/apple` and `gen/android`, which are ignored here: they are derived from `tauri.conf.json` and regenerating them is cheaper than reviewing them. Both platforms build the same `scalar_desktop_lib` crate as the desktop app.

**Not yet verified.** The mobile targets are configured but have not been built or run, because doing so needs Xcode and an Android SDK. Treat them as untested until somebody with those tools says otherwise.

## Layout

```
scripts/build-web.mjs        builds scalar-app/web as static files and copies them into dist/
dist/                        the bundled interface (generated, ignored)
src-tauri/src/lib.rs         everything the app does: api_fetch, the bootstrap script, the
                             menu, navigation handling and window setup
src-tauri/src/main.rs        the desktop binary, which only calls into the library
src-tauri/capabilities/      the permissions the window is granted, kept as small as it can be
src-tauri/tauri.conf.json
```

Keeping the behaviour in the library rather than the binary is what stops the desktop and mobile builds from drifting apart: mobile links the library directly.

## Releases

Pushing a tag such as `v0.1.0` builds a universal `.dmg` for macOS and an installer for Windows and attaches them to a **draft** release, so nothing becomes public until somebody reads it and presses publish. The Windows installer is configured to fetch WebView2 when the machine does not already have it.

Builds are unsigned. macOS will say the developer cannot be verified, and Windows SmartScreen will warn for the same reason. Signing certificates cost money and this project does not spend any; if that changes it will be because somebody with a certificate volunteered it.

## Status

Verified on Windows: the app builds and runs, the menu is attached, window geometry survives a restart, and a second launch focuses the existing window instead of opening another. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are clean, and CI builds on Linux, macOS and Windows.

Not verified: the unit tests could not be run on this machine, because the mingw toolchain cannot link the test harness against WebView2; CI runs them on Linux. Opening an external link in the system browser is covered by unit tests on the rule that decides it, but was not exercised by clicking one, because the interface has no outbound link on its first screen. Binary responses are handled as bytes end to end but no binary endpoint exists to try yet. Nothing has been built or run on macOS, iOS or Android.

## Licence

AGPL-3.0-only. See [LICENSE](LICENSE).
