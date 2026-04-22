//! Manifest types for the ICP application bundle.
//!
//! This is a heavily trimmed down version of the schema in `packaging_design.md`
//! (appendix 1). Only the fields required for the MVP demo are modeled.
//! Unknown fields are accepted and ignored on read, so a richer manifest
//! written by a future version of the format can still be parsed (the extra
//! information will simply be dropped).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The manifest format version this crate writes.
///
/// Readers accept any version, but may not understand fields introduced in
/// newer versions. For the MVP this is fine.
pub const MANIFEST_VERSION: u32 = 1;

/// Top-level application manifest (contents of `manifest.json`).
///
/// The full design has many more optional fields (icons, categories,
/// main_canister, author, license, source_link, ...). For the MVP we keep
/// only what we actually use. Additional fields coming in from the wire
/// are tolerated via serde's default behavior of ignoring unknown keys
/// (we do NOT use `deny_unknown_fields`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Pins the version of the ICP packaged application file format.
    pub manifest_version: u32,

    /// Human readable application name.
    pub name: String,

    /// Optional short version of the application name, for use in
    /// space-constrained surfaces (tiles, menus, etc). Mirrors the
    /// `short_name` field from the full schema in `packaging_design.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,

    /// Optional application version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_version: Option<String>,

    /// Optional application description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional canister name designated as the application's main
    /// entry point. Per the spec this must reference one of the
    /// canisters in [`Manifest::canisters`]; the bundle builder and
    /// reader both validate this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_canister: Option<String>,

    /// Icons that represent the application.
    ///
    /// Spec-wise these are application-level PWA-style icons. This crate
    /// additionally treats an icon whose `src` basename (without
    /// extension) matches a canister name as "that canister's icon" —
    /// see [`crate::Consumer::icons_for`]. Icons that don't follow the
    /// convention are still preserved and exposed via
    /// [`crate::Consumer::icons`]; the mapping is purely additive.
    ///
    /// Each [`Icon::src`] is a path *inside the zip* — typically
    /// `icons/<file>`. The referenced files must be provided to
    /// [`crate::Bundle::create`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<Icon>,

    /// Screenshots advertised to marketplaces / listings. Each entry's
    /// [`Screenshot::src`] is a path *inside the zip* — typically
    /// `screenshots/<file>`. The referenced files must be provided to
    /// [`crate::Bundle::create`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<Screenshot>,

    /// Per-canister configuration.
    pub canisters: BTreeMap<String, CanisterEntry>,
}

impl Manifest {
    /// Returns a new manifest with the current [`MANIFEST_VERSION`] and the
    /// given name, no canisters, and no optional metadata.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            name: name.into(),
            short_name: None,
            application_version: None,
            description: None,
            main_canister: None,
            icons: Vec::new(),
            screenshots: Vec::new(),
            canisters: BTreeMap::new(),
        }
    }
}

/// Canister kind. The full spec has `backend` and `assets`. For the MVP we
/// only install backend canisters; `assets` is accepted but treated exactly
/// like `backend` at install time by any downstream installer. An
/// asset-aware installer is future work.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CanisterKind {
    Backend,
    Assets,
}

impl Default for CanisterKind {
    fn default() -> Self {
        Self::Backend
    }
}

/// One canister in the bundle.
///
/// This exposes the MVP subset the demo users care about: `init_arg`,
/// `upgrade_arg`, `dependencies`, and `env_variables`. See the crate-level
/// docs for the list of fields not yet supported.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanisterEntry {
    /// Canister kind (backend / assets).
    #[serde(default, rename = "type")]
    pub kind: CanisterKind,

    /// Path to the wasm inside the bundle's `canisters/` folder.
    /// For canisters produced by [`crate::Bundle::create`] this is always
    /// `<name>.wasm.gz`.
    pub path: String,

    /// Names of other canisters in the bundle this canister depends on.
    /// Dependencies are used by the installer to compute a topological
    /// installation order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,

    /// Environment variables the installer should set on this canister.
    ///
    /// A value of `None` (serialized as JSON `null`) means "the installer
    /// must prompt the user and fill in a value at install time", per the
    /// design doc. The installer is also expected to inject a
    /// `CANISTER_ID_<name>` variable for every canister in the bundle on
    /// top of these — that is an installer concern, not something the
    /// builder records here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_variables: BTreeMap<String, Option<String>>,

    /// Argument used when the canister is installed for the first time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_arg: Option<CanisterArg>,

    /// Argument used when the canister is upgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_arg: Option<CanisterArg>,
}

/// A candid or json argument, as stored in the manifest.
///
/// We do NOT encode the argument to bytes here — encoding requires knowing
/// the canister's init/upgrade signature, which for a proper implementation
/// should come from an accompanying `.did` file. That is left to the
/// installer. For the MVP we simply preserve the textual form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanisterArg {
    /// Raw textual argument (e.g. `"(42)"` for candid, or a JSON document).
    pub arg: String,

    /// Argument encoding format.
    #[serde(default)]
    pub format: ArgFormat,
}

/// Encoding format for [`CanisterArg`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArgFormat {
    Candid,
    Json,
}

impl Default for ArgFormat {
    fn default() -> Self {
        Self::Candid
    }
}

/// PWA-style purpose tag for an icon. Matches the spec exactly.
///
/// This is independent of this crate's convention of mapping icons to
/// canisters via filename stem — an icon for a canister can still have
/// any `purpose`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IconPurpose {
    Any,
    Maskable,
    Monochrome,
}

/// A single application icon entry.
///
/// Mirrors the shape of the `icons` items in the full schema
/// (`packaging_design.md`). Application-level semantics; this crate adds
/// one extra convention on top: when the basename of `src` (without
/// extension) equals a canister name, the icon is considered to belong
/// to that canister and is returned by [`crate::Consumer::icons_for`].
///
/// `src` is a path *inside the zip file* (e.g. `icons/frontend.png`).
/// The file at that path must be provided as part of the icon payload
/// when calling [`crate::Bundle::create`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Icon {
    /// Path to the icon file inside the zip.
    pub src: String,

    /// Space-separated list of icon dimensions (e.g. `"48x48 72x72"`).
    /// For scalable icons such as SVGs, `"any"` is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<String>,

    /// MIME type (e.g. `"image/png"`). If absent the installer may guess
    /// from the extension.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Purpose of the icon (`any`, `maskable`, `monochrome`). When
    /// absent, defaults to `any` per the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<IconPurpose>,
}

/// Form factor a screenshot was taken on. Matches the spec exactly.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScreenshotFormFactor {
    Narrow,
    Wide,
}

/// A single application screenshot entry.
///
/// MVP scope: we model all the fields from the spec (since they're just
/// metadata) but we do not try to derive `sizes` / `type` automatically.
/// Callers are responsible for filling these in if they need them.
///
/// `src` is a path *inside the zip file* (e.g. `screenshots/hero.png`).
/// The file at that path must be provided as part of the screenshot
/// payload when calling [`crate::Bundle::create`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Screenshot {
    /// Path to the screenshot file inside the zip.
    pub src: String,

    /// Size descriptor (e.g. `"1280x720"`, `"any"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<String>,

    /// MIME type (e.g. `"image/png"`). If absent the installer may guess
    /// from the extension.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Form factor (narrow / wide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_factor: Option<ScreenshotFormFactor>,

    /// Short human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
