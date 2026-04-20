//! MVP packaging crate for ICP applications.
//!
//! This crate implements a *subset* of the format described in
//! `packaging_design.md` at the repo root. It is deliberately minimal: the
//! only per-canister fields supported beyond name/type/path are:
//!
//!  - `init_arg`      — initialization argument (candid text or json string)
//!  - `upgrade_arg`   — upgrade argument (candid text or json string)
//!  - `dependencies`  — names of other canisters this one depends on
//!
//! The bundle file layout on disk matches the design:
//!
//! ```text
//! <app>.icp-app/
//!   manifest.json
//!   canisters/<name>.wasm.gz
//! ```
//!
//! There are two entry points:
//!
//!  - [`Bundle::create`] — given a [`Manifest`] and the wasm bytes for each
//!    canister, writes a zip file to disk.
//!  - [`Bundle::open`]   — given a path to a zip file, extracts/parses the
//!    manifest and exposes the wasm bytes for each canister.
//!
//! Intentionally out of scope for the MVP:
//!   - signing / reproducibility
//!   - asset canister support (`assets/` directory)
//!   - controllers, env variables, cycles, icons, shortcuts
//!   - candid-aware encoding of init/upgrade args
//!   - rigorous manifest schema validation beyond "does it parse?"
//!
//! The `consume` side only extracts the contents; it does not drive the
//! actual canister installation. An installer built on top of this crate
//! would use [`Bundle::open`] and then talk to the IC using `ic-agent` (or
//! the main `icp` crate's facilities) to perform step-by-step install.

pub mod bundle;
pub mod consumer;
pub mod manifest;

pub use bundle::{Bundle, CreateError, OpenError};
pub use consumer::{ConsumeError, Consumer, InstallArg, InstallMode, InstallStep};
pub use manifest::{
    ArgFormat, CanisterArg, CanisterEntry, CanisterKind, Manifest, Screenshot,
    ScreenshotFormFactor, MANIFEST_VERSION,
};
