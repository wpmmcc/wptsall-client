# WPTSALL Client

Local-first WebUI and Desktop clients sharing one Rust core. Configure your own
WPMMCC ATS or WPMMCC WordPress site and provider. No website account is required.

## Build

Use Node.js 22 and stable Rust. Run `npm ci`, `npm run check` and `npm run build`
in `client-wpplugin/source/frontend`. For Desktop, do the same in
`client-desktop/frontend` and install the platform's Tauri 2 build dependencies.

```sh
cargo build --locked --release --bin wptsall-client --manifest-path client-wpplugin/source/Cargo.toml
cargo build --locked --release --manifest-path client-desktop/src-tauri/Cargo.toml
```

WebUI starts with `WPTSALL_WEB_UI=1` and binds to loopback by default. Set
`WPTSALL_DATA_DIR` to a private persistent directory. Desktop runs
`client-desktop/src-tauri/target/release/wptsall-desktop`.

## Distribution

Production CI checks both UIs and Rust crates on six native OS/architecture
runners. The package workflow builds both products, source-free kits, portable
ZIPs and DEB/DMG/NSIS packages. Build artifacts are candidates, not releases or
proof of successful native installation.

Private regression and acceptance suites are maintained in the development
repository, never exported here. Formal release requires default-branch HEAD,
an unused version, green current-commit production workflows and a separately
signed, unexpired full-scope private acceptance document. That document must
cover all twelve native install/OTA/uninstall cells and the product journeys.

The official update channel remains this repository's signed GitHub Releases.
Temporary candidate prereleases do not change the default channel. Read the
first-launch authorization instructions shipped with packages: the packages
do not claim Apple notarization or Microsoft Authenticode signing.

Native uninstall preserves per-user data. Back up your data before updates.
GPL-2.0-or-later, see `LICENSE`.
