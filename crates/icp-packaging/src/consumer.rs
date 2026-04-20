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
//! manifest:
//!
//! | Name                     | Value                                               |
//! |--------------------------|-----------------------------------------------------|
//! | `__CANISTER_NAME`        | name of *this* canister (manifest key)              |
//! | `__CANISTER_DESCRIPTION` | `Manifest.description` (empty string if absent)     |
//! | `__CANISTER_PROJECT`     | `Manifest.name` (the application name)              |
//! | `<DEP>_CANISTER_ID`      | principal of each declared dependency, uppercased   |
//!
//! `<DEP>_CANISTER_ID` is set for every name in
//! [`crate::CanisterEntry::dependencies`], using the canister id supplied
//! via [`Consumer::set_canister_id`]. If a dependency has not yet been
//! assigned an id, [`Consumer::env_variables_for`] / [`Consumer::install_plan`]
//! will return an error — the caller is expected to allocate ids first.

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
    /// Install argument (init or upgrade), ready for the management canister.
    pub arg: Option<InstallArg>,
    /// Whether this should be treated as a first-time install or an upgrade.
    pub mode: InstallMode,
    /// Final environment variable map the caller should apply via
    /// `update_settings`. Includes the synthetic `__CANISTER_*` variables
    /// and `<DEP>_CANISTER_ID` entries.
    pub env_variables: BTreeMap<String, String>,
}

/// Textual install argument carried through from the manifest.
///
/// For the MVP we do not decode the candid/json text into bytes — that
/// requires knowing the canister's candid service signature and is left
/// to the installer. This matches the MVP scope of the builder side.
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

/// Reserved env var keys — the consumer fills these in itself and rejects
/// attempts by the caller to override them.
const RESERVED_ENV_PREFIX: &str = "__CANISTER_";
const RESERVED_ENV_KEYS: &[&str] = &[
    "__CANISTER_NAME",
    "__CANISTER_DESCRIPTION",
    "__CANISTER_PROJECT",
];

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
    /// `null`. Fails if the canister or key is unknown, or if the key is
    /// one of the reserved `__CANISTER_*` slots.
    pub fn provide_env_var(
        &mut self,
        canister: &str,
        key: &str,
        value: impl Into<String>,
    ) -> Result<(), ConsumeError> {
        let entry = self.canister(canister)?;
        if key.starts_with(RESERVED_ENV_PREFIX) || RESERVED_ENV_KEYS.contains(&key) {
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
    ///   - synthetic `__CANISTER_NAME` / `__CANISTER_DESCRIPTION` / `__CANISTER_PROJECT`,
    ///   - `<DEP>_CANISTER_ID` for every declared dependency.
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

        // Synthetic project/description/name. These take precedence over
        // anything the user tried to declare (they can't: provide_env_var
        // rejects reserved keys, and the manifest writer shouldn't set
        // them either — if they did we silently overwrite).
        out.insert("__CANISTER_NAME".to_string(), name.to_string());
        out.insert(
            "__CANISTER_DESCRIPTION".to_string(),
            self.project_description().to_string(),
        );
        out.insert(
            "__CANISTER_PROJECT".to_string(),
            self.project_name().to_string(),
        );

        // <DEP>_CANISTER_ID for every dependency.
        for dep in &entry.dependencies {
            let id = self
                .canister_ids
                .get(dep)
                .ok_or_else(|| ConsumeError::MissingCanisterId {
                    canister: dep.clone(),
                })?;
            out.insert(dep_env_var_key(dep), id.clone());
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
            // be added without breaking the public API.
            let (mode, arg) = (
                InstallMode::Install,
                entry.init_arg.as_ref().map(InstallArg::from),
            );

            plan.push(InstallStep {
                canister_name: name.clone(),
                canister_id,
                wasm,
                arg,
                mode,
                env_variables,
            });
        }

        Ok(plan)
    }
}

/// Naming convention for the dependency-canister-id env var: upper-snake
/// `<NAME>_CANISTER_ID`. Matches how frontend build tooling (e.g. Vite
/// with `VITE_*_CANISTER_ID`) and `ic-cdk` example projects typically
/// read canister ids.
fn dep_env_var_key(canister_name: &str) -> String {
    format!("{}_CANISTER_ID", sanitize_env_key(canister_name))
}

/// Uppercases and replaces anything not matching `[A-Z0-9_]` with `_`.
/// Keeps the output compatible with POSIX env-var naming rules, which is
/// what every env reader we care about expects.
fn sanitize_env_key(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => c.to_ascii_uppercase(),
            'A'..='Z' | '0'..='9' | '_' => c,
            _ => '_',
        })
        .collect()
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
