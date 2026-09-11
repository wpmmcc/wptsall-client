//! Build script — make `cargo build` work on a fresh checkout.
//!
//! static_html.rs embeds `frontend/dist/index.html` at compile time, and
//! that directory is gitignored (a Vite build product). CI and the release
//! packaging build the frontend before cargo, but a plain
//! `cargo build --release` on a fresh clone used to fail with a confusing
//! include_str! error. This script closes that gap:
//!
//! - If `frontend/dist/index.html` already exists: no-op (CI, packaging,
//!   and dev inner loops are untouched).
//! - If it is missing: run `npm ci && npm run build` in `frontend/` once,
//!   the same steps CI performs.
//! - If npm is unavailable or the build fails: fail with an actionable
//!   message instead of a raw include error.

use std::path::Path;
use std::process::Command;

fn main() {
    // Re-embed when the dist output changes (e.g. a dev re-runs vite).
    println!("cargo:rerun-if-changed=frontend/dist/index.html");

    let dist = Path::new("frontend/dist/index.html");
    if dist.exists() {
        return;
    }

    let npm = if cfg!(target_os = "windows") {
        "npm.cmd"
    } else {
        "npm"
    };
    let frontend_dir = Path::new("frontend");

    let run = |args: &[&str], hint: &str| {
        match Command::new(npm).args(args).current_dir(frontend_dir).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("`npm {}` exited with {status}", args.join(" "))),
            Err(_) => Err(format!("could not run `{npm}` {}", hint)),
        }
    };

    // `npm ci` is the CI-equivalent, reproducible install; fall back to
    // `npm install` for throwaway checkouts without the lockfile workflow.
    if run(&["ci", "--no-audit", "--no-fund"], "(is Node.js installed and on PATH?)").is_err() {
        if run(&["install", "--no-audit", "--no-fund"], "").is_err() {
            panic!(
                "frontend/dist/index.html is missing and the WebUI frontend could not be built.\n\
                 The Rust core embeds the Vite build at compile time.\n\
                 Fix: install Node.js, then run `npm ci && npm run build` inside \
                 client-wpplugin/source/frontend and re-run cargo."
            );
        }
    }
    if let Err(err) = run(&["run", "build"], "") {
        panic!(
            "frontend/dist/index.html is missing and `npm run build` failed ({err}).\n\
             The Rust core embeds the Vite build at compile time.\n\
             Fix: run `npm ci && npm run build` inside client-wpplugin/source/frontend, \
             resolve any frontend build errors, then re-run cargo."
        );
    }
    if !dist.exists() {
        panic!(
            "frontend/dist/index.html is still missing after `npm run build`.\n\
             Check the Vite config (vite.config.ts build output) and re-run cargo."
        );
    }
}
