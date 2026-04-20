//! Consumer-side API for installing a bundle.
//!
//! The crate offers two layers of consumption:
//!
//!  1. [`crate::Bundle::open`] / [`crate::Bundle::open_bytes`] — pure I/O:
//!     parse the zip, return the manifest and the decompressed wasms.
//!
//!  2. [`Consumer`] (this module) — a higher-level orchestration helper.
//!     A backend driving the install wires it up like this:
//!
//!     ```text
//!     let consumer = Consumer::from_bytes(multipart_body)?;
//!
//!     // Step 1: ask the bundle which canisters to create.
//!     for name in consumer.canister_names() {
//!         let principal = management_canister.create_canister().await;
//!         consumer.set_canister_id(name, principal)?;
//!     }
//!
//!     // Step 2: optionally fill in env variables the bundle left as `null`.
//!     consumer.provide_env_var("backend", "API_KEY", "xyz")?;
//!
//!     // Step 3: iterate the install plan in topological order.
//!     for step in consumer.install_plan()? {
//!         management_canister
//!             .install_code(step.canister_id, step.wasm, step.arg, step.mode)
//!             .await;
//!         management_canister
//!             .update_settings(step.canister_id, step.env_variables)
//!             .await;
//!     }
//!     ```
//!
//! The backend (or whatever is orchestrating) is expected to drive the IC
//! side itself; this crate does not pull in `ic-agent`. The [`Consumer`]
//! just exposes the information needed to call `create_canister`,
//! `update_settings`, and `install_code` correctly.
//!
//! # Synthetic environment variables
//!
//! For every canister in the bundle the consumer injects a handful of
//! built-in environment variables on top of anything declared in the
//! manifest. There are two groups:
//!
//! ## Metadata (`__META_*`) — internal-only
//!
//! Stored on the canister but intentionally *not* exposed to the
//! frontend (the `ic_env` cookie only surfaces `PUBLIC_*` keys). The
//! canister itself can read these with `ic_env::get_env_var` etc.
//!
//! | Name                | Value                                            |
//! |---------------------|--------------------------------------------------|
//! | `__META_NAME`       | name of *this* canister (manifest key)           |
//! | `__META_DESCRIPTION`| `Manifest.description` (empty string if absent)  |
//! | `__META_PROJECT`    | `Manifest.name` (the application name)           |
//!
//! ## Dependency ids (`PUBLIC_CANISTER_ID:<dep>`) — frontend-visible
//!
//! For every dependency declared in the canister's manifest entry, one
//! key of the form `PUBLIC_CANISTER_ID:<dep>`. The name is kept verbatim
//! from the manifest; a literal colon separates the prefix from the
//! name. This matches the convention consumed by frontends built against
//! the icp-cli asset canister (wasm >= 0.30.2), which read values out of
//! the `ic_env` cookie with:
//!
//! ```text
//! URLSearchParams.get("PUBLIC_CANISTER_ID:backend")
//! ```
//!
//! If a dependency has not yet been assigned an id,
//! [`Consumer::env_variables_for`] / [`Consumer::install_plan`] will
//! return an error — the caller is expected to allocate ids first.
//!
//! Both `__META_` and `PUBLIC_CANISTER_` are reserved prefixes:
//! [`Consumer::provide_env_var`] refuses keys that start with either.

use std::collections::BTreeMap;

use snafu::{ResultExt, Snafu};

use crate::bundle::{Bundle, OpenError};
use crate::manifest::{ArgFormat, CanisterArg, CanisterEntry, Manifest};

/// Errors produced by the [`Consumer`] API.
#[derive(Debug, Snafu)]
pub enum ConsumeError {
    #[snafu(display("failed to open bundle"))]
    Open { source: OpenError },

    #[snafu(display("canister '{canister}' is not part of the bundle"))]
    UnknownCanister { canister: String },

    #[snafu(display(
        "canister '{canister}' has not been assigned a canister id yet; \
         call set_canister_id first"
    ))]
    MissingCanisterId { canister: String },

    #[snafu(display(
        "canister '{canister}' environment variable '{key}' was declared as \
         null in the manifest (installer must prompt) but no value has been \
         supplied; call provide_env_var first"
    ))]
    MissingEnvVar { canister: String, key: String },

    #[snafu(display("environment variable key '{key}' for canister '{canister}' is reserved"))]
    ReservedEnvVar { canister: String, key: String },

    #[snafu(display(
        "canister '{canister}' declares an environment variable key '{key}' \
         that is not part of its manifest entry"
    ))]
    UnknownEnvVar { canister: String, key: String },

    #[snafu(display("manifest has a dependency cycle involving '{canister}'"))]
    DependencyCycle { canister: String },

    #[snafu(display("canister '{canister}' init/upgrade arg is not valid candid"))]
    CandidParse {
        canister: String,
        source: candid_parser::Error,
    },

    #[snafu(display("canister '{canister}' init/upgrade arg failed to encode to candid bytes"))]
    CandidEncode {
        canister: String,
        source: candid::Error,
    },

    #[snafu(display(
        "canister '{canister}' has a json-format arg; encoding JSON args requires the \
         canister's candid service signature and is not supported by this crate"
    ))]
    JsonArgUnsupported { canister: String },

    #[snafu(display("canister '{canister}' asset '{path}' is not part of the bundle"))]
    UnknownAsset { canister: String, path: String },
}

/// Which install flavor should be performed for a given canister.
///
/// We determine this purely from the manifest + supplied state; the caller
/// maps it to the concrete management-canister `install_code` mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMode {
    /// Fresh install — the canister is being created from scratch.
    Install,
    /// Upgrade — the canister existed before this bundle application.
    ///
    /// The current MVP never yields this variant because we don't track
    /// previously-installed state; it's wired up so a future "upgrade" API
    /// can slot in cleanly.
    Upgrade,
    /// Reinstall — discard state and reinstall.
    Reinstall,
}

/// One unit of work the caller should perform.
///
/// Produced by [`Consumer::install_plan`]. The vector is in a valid
/// topological order given the bundle's `dependencies` graph, so it can be
/// consumed front-to-back without further sorting.
#[derive(Clone, Debug)]
pub struct InstallStep {
    /// Canister name as declared in the manifest.
    pub canister_name: String,
    /// Canister id the caller allocated (via `set_canister_id`).
    pub canister_id: String,
    /// Raw wasm bytes to install.
    pub wasm: Vec<u8>,
    /// Install argument (init or upgrade), already encoded to the bytes
    /// the management canister expects.
    ///
    /// For candid-format args the text from the manifest is parsed with
    /// `candid_parser` and re-encoded via `IDLArgs::to_bytes()`. JSON-
    /// format args are rejected at plan time (see [`ConsumeError`]).
    ///
    /// If the caller wants the original text (e.g. for logging), see
    /// [`InstallStep::raw_arg`].
    pub arg: Option<Vec<u8>>,
    /// Original textual form of the arg, preserved for logging / debugging.
    pub raw_arg: Option<InstallArg>,
    /// Whether this should be treated as a first-time install or an upgrade.
    pub mode: InstallMode,
    /// Final environment variable map the caller should apply via
    /// `update_settings`. Includes the synthetic `__META_*` metadata
    /// variables and the `PUBLIC_CANISTER_ID:<dep>` entries.
    pub env_variables: BTreeMap<String, String>,
}

/// Textual install argument carried through from the manifest.
///
/// This is kept around alongside the pre-encoded bytes in
/// [`InstallStep`] so that callers that want to log the arg in a human
/// readable form still can.
#[derive(Clone, Debug)]
pub struct InstallArg {
    pub arg: String,
    pub format: ArgFormat,
}

impl From<&CanisterArg> for InstallArg {
    fn from(v: &CanisterArg) -> Self {
        Self {
            arg: v.arg.clone(),
            format: v.format,
        }
    }
}

impl InstallArg {
    /// Encode this argument to the raw bytes the IC management canister
    /// accepts in `install_code`.
    ///
    ///  - [`ArgFormat::Candid`]: parsed with `candid_parser::parse_idl_args`
    ///    and encoded via `IDLArgs::to_bytes`. Does not require a `.did`
    ///    file — the IC will type-check the resulting bytes against the
    ///    canister's init signature on install.
    ///  - [`ArgFormat::Json`]: unsupported without the canister's candid
    ///    service signature; returns [`ConsumeError::JsonArgUnsupported`].
    pub fn to_bytes(&self, canister: &str) -> Result<Vec<u8>, ConsumeError> {
        match self.format {
            ArgFormat::Candid => {
                let parsed = candid_parser::parse_idl_args(self.arg.trim()).map_err(|source| {
                    ConsumeError::CandidParse {
                        canister: canister.to_string(),
                        source,
                    }
                })?;
                parsed
                    .to_bytes()
                    .map_err(|source| ConsumeError::CandidEncode {
                        canister: canister.to_string(),
                        source,
                    })
            }
            ArgFormat::Json => JsonArgUnsupportedSnafu {
                canister: canister.to_string(),
            }
            .fail(),
        }
    }
}

/// High-level consumer-side view of a bundle.
///
/// Construct one via [`Consumer::from_path`] or [`Consumer::from_bytes`],
/// feed it the ids allocated for each canister, optionally provide values
/// for env variables the manifest marked as `null`, then pull out an
/// install plan. The caller is expected to drive the actual IC API calls.
#[derive(Debug)]
pub struct Consumer {
    bundle: Bundle,

    /// Canister name -> allocated principal (stored as text).
    canister_ids: BTreeMap<String, String>,

    /// User-supplied values for env variables the manifest left as `null`,
    /// keyed by (canister, var name).
    supplied_env_vars: BTreeMap<(String, String), String>,
}

/// Reserved env var key prefixes. `provide_env_var` rejects any key that
/// starts with one of these so the caller can't shadow a synthetic slot.
///
/// - `__META_` — bundle/canister metadata (see `env_variables_for`).
/// - `PUBLIC_CANISTER_` — dependency-canister-id keys that the asset
///   canister would surface to the frontend via `ic_env`.
const RESERVED_ENV_PREFIXES: &[&str] = &["__META_", "PUBLIC_CANISTER_"];

/// Key prefix used to expose a dependency's canister id. Matches the
/// convention the icp-cli asset canister serves via the `ic_env` cookie
/// (frontend reads `URLSearchParams.get("PUBLIC_CANISTER_ID:backend")`).
const PUBLIC_CANISTER_ID_PREFIX: &str = "PUBLIC_CANISTER_ID:";

impl Consumer {
    /// Construct a consumer from a bundle file on disk.
    pub fn from_path(path: &std::path::Path) -> Result<Self, ConsumeError> {
        let bundle = Bundle::open(path).context(OpenSnafu)?;
        Ok(Self::from_bundle(bundle))
    }

    /// Construct a consumer from in-memory bundle bytes, e.g. multipart
    /// form-data body.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConsumeError> {
        let bundle = Bundle::open_bytes(bytes).context(OpenSnafu)?;
        Ok(Self::from_bundle(bundle))
    }

    /// Construct a consumer from an already-opened [`Bundle`].
    pub fn from_bundle(bundle: Bundle) -> Self {
        Self {
            bundle,
            canister_ids: BTreeMap::new(),
            supplied_env_vars: BTreeMap::new(),
        }
    }

    // --- Getters --------------------------------------------------------

    /// Application name (`manifest.name`).
    pub fn project_name(&self) -> &str {
        &self.bundle.manifest.name
    }

    /// Bundle description (`manifest.description`). Empty string if absent.
    pub fn project_description(&self) -> &str {
        self.bundle.manifest.description.as_deref().unwrap_or("")
    }

    /// The full underlying manifest, if the caller wants the raw view.
    pub fn manifest(&self) -> &Manifest {
        &self.bundle.manifest
    }

    /// Names of all canisters in the bundle, in manifest (sorted) order.
    pub fn canister_names(&self) -> Vec<String> {
        self.bundle.manifest.canisters.keys().cloned().collect()
    }

    /// Access a single canister entry by name.
    pub fn canister(&self, name: &str) -> Result<&CanisterEntry, ConsumeError> {
        self.bundle
            .manifest
            .canisters
            .get(name)
            .ok_or_else(|| ConsumeError::UnknownCanister {
                canister: name.to_string(),
            })
    }

    /// Raw wasm bytes for a canister. These are already gunzipped.
    pub fn canister_wasm(&self, name: &str) -> Result<&[u8], ConsumeError> {
        self.bundle
            .wasms
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| ConsumeError::UnknownCanister {
                canister: name.to_string(),
            })
    }

    /// List of canister names that the given canister depends on.
    pub fn dependencies_of(&self, name: &str) -> Result<&[String], ConsumeError> {
        Ok(self.canister(name)?.dependencies.as_slice())
    }

    /// Screenshot bytes keyed by `src` path. The caller can use these to
    /// populate a listing page before or after install.
    pub fn screenshots(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.bundle.screenshots
    }

    // --- Assets --------------------------------------------------------

    /// Names of canisters that carry bundled asset files.
    ///
    /// Only canisters whose manifest type is `assets` *and* which actually
    /// have at least one file are listed. A caller iterating this list
    /// can decide, per canister, whether to push to the asset canister.
    pub fn canisters_with_assets(&self) -> Vec<String> {
        self.bundle
            .assets
            .iter()
            .filter(|(_, files)| !files.is_empty())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// List the relative paths of every asset file bundled for a given
    /// canister. Returns an empty slice when the canister has no assets.
    ///
    /// The returned paths are exactly the keys a caller should use when
    /// pushing content to the asset canister (typically they form the
    /// asset URL, e.g. `index.html`, `static/logo.png`).
    pub fn asset_paths(&self, canister: &str) -> Result<Vec<String>, ConsumeError> {
        // Validate the canister exists even if it has no assets, so the
        // caller gets a clear error for typos.
        self.canister(canister)?;
        Ok(self
            .bundle
            .assets
            .get(canister)
            .map(|files| files.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// Fetch the raw bytes for a single asset file.
    ///
    /// The caller is expected to set an appropriate `Content-Type` and
    /// push this to the asset canister using the `identity` content
    /// encoding (per `packaging_design.md`), optionally also gzipping for
    /// a `gzip` content-encoding variant.
    pub fn asset_bytes(&self, canister: &str, path: &str) -> Result<&[u8], ConsumeError> {
        self.canister(canister)?;
        self.bundle
            .assets
            .get(canister)
            .and_then(|files| files.get(path))
            .map(Vec::as_slice)
            .ok_or_else(|| ConsumeError::UnknownAsset {
                canister: canister.to_string(),
                path: path.to_string(),
            })
    }

    /// Return every asset file for a canister as a map of
    /// `relative_path -> bytes`. Convenient when the caller wants to
    /// stream everything to the asset canister in a batch.
    ///
    /// Returns an empty map if the canister has no bundled assets.
    pub fn all_assets_for(
        &self,
        canister: &str,
    ) -> Result<&BTreeMap<String, Vec<u8>>, ConsumeError> {
        self.canister(canister)?;
        static EMPTY: std::sync::OnceLock<BTreeMap<String, Vec<u8>>> = std::sync::OnceLock::new();
        Ok(self
            .bundle
            .assets
            .get(canister)
            .unwrap_or_else(|| EMPTY.get_or_init(BTreeMap::new)))
    }

    /// Which env-var keys the manifest declared as `null` for a canister.
    /// These must all be supplied via [`Self::provide_env_var`] before a
    /// plan can be produced.
    pub fn pending_env_vars(&self, name: &str) -> Result<Vec<String>, ConsumeError> {
        Ok(self
            .canister(name)?
            .env_variables
            .iter()
            .filter_map(|(k, v)| if v.is_none() { Some(k.clone()) } else { None })
            .collect())
    }

    // --- Mutators -------------------------------------------------------

    /// Record the canister id that was allocated for a given canister.
    ///
    /// The id is stored as the text (principal) form; the caller is free
    /// to pass whatever their IC client uses as long as it round-trips.
    pub fn set_canister_id(
        &mut self,
        name: &str,
        id: impl Into<String>,
    ) -> Result<(), ConsumeError> {
        if !self.bundle.manifest.canisters.contains_key(name) {
            return UnknownCanisterSnafu {
                canister: name.to_string(),
            }
            .fail();
        }
        self.canister_ids.insert(name.to_string(), id.into());
        Ok(())
    }

    /// Provide a value for an environment variable the manifest left as
    /// `null`. Fails if the canister or key is unknown, or if the key
    /// starts with one of the reserved prefixes (`__META_`,
    /// `PUBLIC_CANISTER_`). See the module-level docs for the full list
    /// of synthetic variables the consumer manages itself.
    pub fn provide_env_var(
        &mut self,
        canister: &str,
        key: &str,
        value: impl Into<String>,
    ) -> Result<(), ConsumeError> {
        let entry = self.canister(canister)?;
        if RESERVED_ENV_PREFIXES.iter().any(|p| key.starts_with(p)) {
            return ReservedEnvVarSnafu {
                canister: canister.to_string(),
                key: key.to_string(),
            }
            .fail();
        }
        if !entry.env_variables.contains_key(key) {
            return UnknownEnvVarSnafu {
                canister: canister.to_string(),
                key: key.to_string(),
            }
            .fail();
        }
        self.supplied_env_vars
            .insert((canister.to_string(), key.to_string()), value.into());
        Ok(())
    }

    // --- Derived views --------------------------------------------------

    /// Compute the final environment variable map for a single canister.
    ///
    /// This is the full map that should be installed on the canister via
    /// `update_settings`. It includes:
    ///   - user-declared variables from the manifest (with `null`s resolved
    ///     from [`Self::provide_env_var`]),
    ///   - `__META_NAME` / `__META_DESCRIPTION` / `__META_PROJECT`
    ///     (bundle/canister metadata; kept internal to the canister),
    ///   - `PUBLIC_CANISTER_ID:<dep>` for every declared dependency
    ///     (surfaced to the frontend via `ic_env`).
    ///
    /// Fails if any dependency's canister id has not been set, or if a
    /// `null` manifest variable has not been resolved.
    pub fn env_variables_for(&self, name: &str) -> Result<BTreeMap<String, String>, ConsumeError> {
        let entry = self.canister(name)?;
        let mut out: BTreeMap<String, String> = BTreeMap::new();

        // User-declared manifest variables.
        for (k, v) in &entry.env_variables {
            let value = match v {
                Some(s) => s.clone(),
                None => self
                    .supplied_env_vars
                    .get(&(name.to_string(), k.to_string()))
                    .cloned()
                    .ok_or_else(|| ConsumeError::MissingEnvVar {
                        canister: name.to_string(),
                        key: k.clone(),
                    })?,
            };
            out.insert(k.clone(), value);
        }

        // Internal metadata. These take precedence over anything the
        // caller tried to declare: provide_env_var rejects reserved
        // prefixes, and the manifest writer shouldn't set them either
        // — if they did we silently overwrite.
        out.insert("__META_NAME".to_string(), name.to_string());
        out.insert(
            "__META_DESCRIPTION".to_string(),
            self.project_description().to_string(),
        );
        out.insert(
            "__META_PROJECT".to_string(),
            self.project_name().to_string(),
        );

        // PUBLIC_CANISTER_ID:<dep> for every dependency. This format
        // matches what the icp-cli asset canister serves via the ic_env
        // cookie and what frontends read at runtime.
        for dep in &entry.dependencies {
            let id = self
                .canister_ids
                .get(dep)
                .ok_or_else(|| ConsumeError::MissingCanisterId {
                    canister: dep.clone(),
                })?;
            out.insert(public_canister_id_key(dep), id.clone());
        }

        // Synthetic project/description/name. These take precedence over
        // anything the user tried to declare (they can't: provide_env_var
        // rejects reserved keys, and the manifest writer shouldn't set
        // them either — if they did we silently overwrite).
        out.insert("PUBLIC_CANISTER_NAME".to_string(), name.to_string());
        out.insert(
            "PUBLIC_CANISTER_DESCRIPTION".to_string(),
            self.project_description().to_string(),
        );
        out.insert(
            "PUBLIC_CANISTER_PROJECT".to_string(),
            self.project_name().to_string(),
        );

        // PUBLIC_CANISTER_ID:<dep> for every dependency. The key format
        // matches what the icp-cli asset canister serves via the ic_env
        // cookie and what frontends read at runtime.
        for dep in &entry.dependencies {
            let id = self
                .canister_ids
                .get(dep)
                .ok_or_else(|| ConsumeError::MissingCanisterId {
                    canister: dep.clone(),
                })?;
            out.insert(public_canister_id_key(dep), id.clone());
        }

        Ok(out)
    }

    /// Return the canister install order: a list of canister names sorted
    /// so that every canister appears after all of its dependencies.
    ///
    /// This is what the caller should drive `install_code` with. Fails if
    /// the dependency graph has a cycle.
    pub fn install_order(&self) -> Result<Vec<String>, ConsumeError> {
        topo_sort(&self.bundle.manifest.canisters)
    }

    /// Produce the complete install plan, ready for the caller to drive
    /// against the management canister.
    ///
    /// Every canister in the bundle must have had its id set via
    /// [`Self::set_canister_id`], and every `null` env variable must have
    /// been resolved via [`Self::provide_env_var`]. If anything is
    /// missing, the relevant error is returned.
    pub fn install_plan(&self) -> Result<Vec<InstallStep>, ConsumeError> {
        let order = self.install_order()?;

        // Ensure everyone has an id before we start materializing steps —
        // a partial plan is more confusing than a single up-front error.
        for name in &order {
            if !self.canister_ids.contains_key(name) {
                return MissingCanisterIdSnafu {
                    canister: name.clone(),
                }
                .fail();
            }
        }

        let mut plan = Vec::with_capacity(order.len());
        for name in &order {
            let entry = self
                .bundle
                .manifest
                .canisters
                .get(name)
                .expect("present: iterating manifest keys");
            let wasm = self
                .bundle
                .wasms
                .get(name)
                .expect("Bundle::open guarantees a wasm per canister")
                .clone();

            let canister_id = self
                .canister_ids
                .get(name)
                .expect("presence checked above")
                .clone();

            let env_variables = self.env_variables_for(name)?;

            // MVP: we always treat a bundle application as a fresh install.
            // The `mode` field is here so a future "upgrade" code path can
            // be added without breaking the public API. For an install we
            // use init_arg; an upgrade would use upgrade_arg.
            let mode = InstallMode::Install;
            let raw_arg = entry.init_arg.as_ref().map(InstallArg::from);
            let arg = raw_arg.as_ref().map(|a| a.to_bytes(name)).transpose()?;

            plan.push(InstallStep {
                canister_name: name.clone(),
                canister_id,
                wasm,
                arg,
                raw_arg,
                mode,
                env_variables,
            });
        }

        Ok(plan)
    }
}

/// Naming convention for the dependency-canister-id env var:
/// `PUBLIC_CANISTER_ID:<name>`. The name is kept in the manifest's
/// original casing (lowercase in practice, by convention), and the key
/// includes a literal colon separator.
///
/// This matches the convention consumed by the icp-cli asset canister's
/// `ic_env` cookie (wasm >= 0.30.2) and by frontends reading
/// `URLSearchParams.get("PUBLIC_CANISTER_ID:<name>")`.
fn public_canister_id_key(canister_name: &str) -> String {
    format!("{PUBLIC_CANISTER_ID_PREFIX}{canister_name}")
}

/// Topological sort of the `canisters` map on the `dependencies` edges.
/// Preserves alphabetical order among nodes at the same "level" thanks to
/// BTreeMap's sorted iteration.
fn topo_sort(canisters: &BTreeMap<String, CanisterEntry>) -> Result<Vec<String>, ConsumeError> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        InStack,
        Done,
    }

    fn dfs(
        node: &str,
        canisters: &BTreeMap<String, CanisterEntry>,
        marks: &mut BTreeMap<String, Mark>,
        out: &mut Vec<String>,
    ) -> Result<(), ConsumeError> {
        match marks.get(node).copied().unwrap_or(Mark::Unseen) {
            Mark::Done => return Ok(()),
            Mark::InStack => {
                return DependencyCycleSnafu {
                    canister: node.to_string(),
                }
                .fail();
            }
            Mark::Unseen => {}
        }
        marks.insert(node.to_string(), Mark::InStack);
        if let Some(entry) = canisters.get(node) {
            for dep in &entry.dependencies {
                dfs(dep, canisters, marks, out)?;
            }
        }
        marks.insert(node.to_string(), Mark::Done);
        out.push(node.to_string());
        Ok(())
    }

    let mut marks: BTreeMap<String, Mark> = BTreeMap::new();
    let mut out: Vec<String> = Vec::with_capacity(canisters.len());
    for name in canisters.keys() {
        dfs(name, canisters, &mut marks, &mut out)?;
    }
    Ok(out)
}
