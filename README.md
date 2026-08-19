<div align="center">
  <img src="https://raw.githubusercontent.com/scalar-app/.github/main/profile/assets/scalar.png" alt="Scalar" width="96" />
  <h1>Scalar for desktop and mobile</h1>
  <p>macOS, Windows, Linux, iOS and Android, from one codebase.</p>
</div>

---

This is the native shell around Scalar. It is not a second interface: it loads the same web app, built as static files and bundled inside the app, so a change to a screen reaches every platform without being written twice.

Tauri 2 is the reason one repository can cover all five targets. The Rust core here builds as a binary on desktop and as a library on iOS and Android, and the interface is a system webview rather than a bundled browser, which is why the download is measured in megabytes rather than hundreds of them.

## What the native part actually does

Almost nothing, deliberately. There is one job it exists for.

A packaged app is served from `tauri://localhost`, so every call to a Scalar server is cross origin. The session cookie is `HttpOnly` and `SameSite=Lax`, which means a webview will not send it, and the API's CORS allowlist would not name that origin in any case. The alternatives were to weaken the cookie to `SameSite=None` or to widen CORS on every self-hosted server, both of which make the browser deployment less safe to fix something that is not the browser's problem.

Instead, requests are made from Rust, where an ordinary HTTP client with its own cookie jar applies. `src-tauri/src/lib.rs` exposes a single `api_fetch` command and injects a `window.__SCALAR_FETCH__` shim before the app boots. The web app asks the SDK for a custom fetch, which it already supports, and never learns it is running natively. **The API is unchanged.**

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
scripts/build-web.mjs   builds scalar-app/web as static files and copies them into dist/
dist/                   the bundled interface (generated, ignored)
src-tauri/src/lib.rs    api_fetch and the bootstrap script; everything the app does
src-tauri/src/main.rs   the desktop binary, which only calls into the library
src-tauri/tauri.conf.json
```

Keeping the behaviour in the library rather than the binary is what stops the desktop and mobile builds from drifting apart: mobile links the library directly.

## Status

Desktop is built in CI on Linux, macOS and Windows. There are no packaged releases yet, and the app is not signed or notarized, so macOS and Windows will warn about an unidentified developer. Signing costs money and this project does not spend any; if that changes it will be because somebody with a certificate volunteered it.

## Licence

AGPL-3.0-only. See [LICENSE](LICENSE).
