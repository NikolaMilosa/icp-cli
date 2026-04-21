use std::collections::BTreeMap;

use anyhow::{Context as _, anyhow};
use clap::Args;
use icp::context::{Context, EnvironmentSelection};
use icp::prelude::{Path, PathBuf};
use icp_packaging::{
    ArgFormat, Bundle, CanisterArg, CanisterEntry, CanisterKind, Manifest, Screenshot,
    ScreenshotFormFactor, bundle::default_wasm_path,
};
use serde::Deserialize;
use tracing::info;

use crate::options::EnvironmentOpt;

/// Create an application bundle (.icp-app zip) from a build manifest JSON file.
///
/// The build manifest describes everything that goes into the bundle:
/// application metadata, per-canister configuration (init/upgrade args,
/// dependencies, env variables), screenshots, and asset directories. Its
/// shape mirrors the runtime `manifest.json` that ends up inside the zip,
/// with a handful of extra fields that only make sense at build time
/// (`asset_dir`, optional `wasm`, screenshot `src` points at a file on
/// disk).
///
/// Wasm bytes for each canister are resolved in this order:
///   1. `canisters.<name>.wasm` in the build manifest, if set.
///   2. Otherwise, the local build artifact store (populated by
///      `icp build`). The `-e <environment>` flag selects the environment
///      to read from.
///
/// See `docs/package-manifest.md` for a full example.
#[derive(Debug, Args)]
pub(crate) struct CreateArgs {
    /// Path to the build manifest JSON file.
    #[arg(long, short = 'm')]
    pub(crate) manifest: PathBuf,

    /// Output path for the generated bundle zip.
    #[arg(long, short = 'o')]
    pub(crate) out: PathBuf,

    /// Environment whose artifact store should be consulted when a
    /// canister doesn't specify a `wasm` path in the manifest. Defaults
    /// to the local environment.
    #[command(flatten)]
    pub(crate) environment: EnvironmentOpt,
}

// --- Build-manifest schema -------------------------------------------------
//
// This mirrors `icp_packaging::Manifest` with a few additions:
//   - manifest_version is optional (defaults to 1)
//   - canisters.<name>.wasm: optional path to a wasm file on disk
//   - canisters.<name>.asset_dir: optional path to a directory whose
//     contents get packed as this canister's asset payload. Presence of
//     this field implies `type: assets`.
//   - screenshots[].src is interpreted as a path on disk; it gets rewritten
//     to `screenshots/<basename>` inside the zip.
//
// Unknown fields are rejected so typos surface quickly.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildManifest {
    #[serde(default)]
    manifest_version: Option<u32>,

    name: String,

    #[serde(default)]
    short_name: Option<String>,

    #[serde(default)]
    application_version: Option<String>,

    #[serde(default)]
    description: Option<String>,

    #[serde(default)]
    screenshots: Vec<BuildScreenshot>,

    canisters: BTreeMap<String, BuildCanister>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildCanister {
    /// Optional: `backend` (default) or `assets`. Setting `asset_dir`
    /// automatically promotes this to `assets`.
    #[serde(default, rename = "type")]
    kind: Option<CanisterKindSpec>,

    /// Optional path to a wasm file on disk. Overrides the artifact store
    /// lookup.
    #[serde(default)]
    wasm: Option<String>,

    /// Optional path to a directory whose contents should be packed as
    /// this canister's asset payload.
    #[serde(default)]
    asset_dir: Option<String>,

    #[serde(default)]
    dependencies: Vec<String>,

    #[serde(default)]
    env_variables: BTreeMap<String, Option<String>>,

    #[serde(default)]
    init_arg: Option<BuildCanisterArg>,

    #[serde(default)]
    upgrade_arg: Option<BuildCanisterArg>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum CanisterKindSpec {
    Backend,
    Assets,
}

impl From<CanisterKindSpec> for CanisterKind {
    fn from(v: CanisterKindSpec) -> Self {
        match v {
            CanisterKindSpec::Backend => CanisterKind::Backend,
            CanisterKindSpec::Assets => CanisterKind::Assets,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildCanisterArg {
    arg: String,
    #[serde(default)]
    format: Option<ArgFormatSpec>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ArgFormatSpec {
    Candid,
    Json,
}

impl From<ArgFormatSpec> for ArgFormat {
    fn from(v: ArgFormatSpec) -> Self {
        match v {
            ArgFormatSpec::Candid => ArgFormat::Candid,
            ArgFormatSpec::Json => ArgFormat::Json,
        }
    }
}

/// Screenshot entry in the build manifest. `src` is a path on disk; the
/// builder reads the bytes and rewrites `src` to its in-zip location.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildScreenshot {
    src: String,
    #[serde(default)]
    sizes: Option<String>,
    #[serde(default, rename = "type")]
    mime_type: Option<String>,
    #[serde(default)]
    form_factor: Option<ScreenshotFormFactorSpec>,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ScreenshotFormFactorSpec {
    Narrow,
    Wide,
}

impl From<ScreenshotFormFactorSpec> for ScreenshotFormFactor {
    fn from(v: ScreenshotFormFactorSpec) -> Self {
        match v {
            ScreenshotFormFactorSpec::Narrow => ScreenshotFormFactor::Narrow,
            ScreenshotFormFactorSpec::Wide => ScreenshotFormFactor::Wide,
        }
    }
}

// --- Command entry point ---------------------------------------------------

pub(crate) async fn exec(ctx: &Context, args: &CreateArgs) -> Result<(), anyhow::Error> {
    // 1. Load + parse the build manifest.
    let manifest_bytes = std::fs::read(args.manifest.as_std_path())
        .with_context(|| format!("failed to read build manifest '{}'", args.manifest))?;
    let build: BuildManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to parse build manifest '{}'", args.manifest))?;

    // All paths in the build manifest are resolved relative to the
    // manifest file's own directory. This keeps the manifest portable —
    // you can check it in next to the files it references.
    let manifest_dir: PathBuf = args
        .manifest
        .parent()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| PathBuf::from("."));

    if build.canisters.is_empty() {
        return Err(anyhow!(
            "build manifest '{}' declares no canisters",
            args.manifest
        ));
    }

    // 2. Load the environment lazily. We only need it when at least one
    //    canister omits its `wasm` field. Wrap in an Option so we don't
    //    fail if no project is set up and every canister specifies a wasm.
    let need_artifact_store = build.canisters.values().any(|c| c.wasm.is_none());
    let env_opt = if need_artifact_store {
        let environment_selection: EnvironmentSelection = args.environment.clone().into();
        Some(ctx.get_environment(&environment_selection).await?)
    } else {
        None
    };

    // 3. Build the output manifest + wasm/asset/screenshot maps.
    let mut out_manifest = Manifest::new(&build.name);
    out_manifest.manifest_version = build
        .manifest_version
        .unwrap_or(icp_packaging::MANIFEST_VERSION);
    out_manifest.short_name = build.short_name.clone();
    out_manifest.application_version = build.application_version.clone();
    out_manifest.description = build.description.clone();

    let mut wasms: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut assets: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();

    let canister_names: Vec<String> = build.canisters.keys().cloned().collect();

    for (name, canister) in &build.canisters {
        // Dependencies must refer to canisters in the same manifest.
        for dep in &canister.dependencies {
            if !canister_names.contains(dep) {
                return Err(anyhow!(
                    "canister '{name}' depends on '{dep}' which is not in the manifest"
                ));
            }
        }

        // Resolve the wasm bytes.
        let wasm = match canister.wasm.as_deref() {
            Some(rel_path) => {
                let abs = resolve_relative(&manifest_dir, rel_path);
                std::fs::read(abs.as_std_path())
                    .with_context(|| format!("failed to read wasm for canister '{name}' at '{abs}'"))?
            }
            None => {
                let env = env_opt
                    .as_ref()
                    .expect("need_artifact_store is true when any canister omits wasm");
                if !env.canisters.contains_key(name) {
                    return Err(anyhow!(
                        "canister '{name}' has no `wasm` in the manifest and is not \
                         part of environment '{}'",
                        env.name
                    ));
                }
                ctx.artifacts.lookup(name).await.with_context(|| {
                    format!("no build artifact for canister '{name}'; run `icp build` first")
                })?
            }
        };

        // Resolve asset files, if any.
        let kind = if let Some(dir) = canister.asset_dir.as_deref() {
            let abs = resolve_relative(&manifest_dir, dir);
            let files = collect_asset_dir(&abs).with_context(|| {
                format!("failed to collect assets for canister '{name}' from '{abs}'")
            })?;
            assets.insert(name.clone(), files);
            CanisterKind::Assets
        } else {
            canister.kind.map(CanisterKind::from).unwrap_or_default()
        };

        // Translate init/upgrade args.
        let init_arg = canister.init_arg.as_ref().map(to_canister_arg);
        let upgrade_arg = canister.upgrade_arg.as_ref().map(to_canister_arg);

        let entry = CanisterEntry {
            kind,
            path: default_wasm_path(name),
            dependencies: canister.dependencies.clone(),
            env_variables: canister.env_variables.clone(),
            init_arg,
            upgrade_arg,
        };
        out_manifest.canisters.insert(name.clone(), entry);
        wasms.insert(name.clone(), wasm);
    }

    // 4. Read and attach screenshots. Source path -> bytes; the in-zip
    //    path is always `screenshots/<basename>` with an auto-disambiguator
    //    on collisions.
    let mut screenshot_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut used_names: std::collections::BTreeSet<String> = Default::default();
    for spec in &build.screenshots {
        let abs = resolve_relative(&manifest_dir, &spec.src);
        let bytes = std::fs::read(abs.as_std_path())
            .with_context(|| format!("failed to read screenshot '{abs}'"))?;

        let file_name = abs
            .file_name()
            .ok_or_else(|| anyhow!("screenshot path '{abs}' has no file name"))?;
        let mut candidate = format!("screenshots/{file_name}");
        let mut n: u32 = 1;
        while used_names.contains(&candidate) {
            n += 1;
            candidate = format!("screenshots/{n}-{file_name}");
        }
        used_names.insert(candidate.clone());

        let mime_type = spec.mime_type.clone().or_else(|| guess_mime_type(&abs));

        out_manifest.screenshots.push(Screenshot {
            src: candidate.clone(),
            sizes: spec.sizes.clone(),
            mime_type,
            form_factor: spec.form_factor.map(ScreenshotFormFactor::from),
            label: spec.label.clone(),
        });
        screenshot_bytes.insert(candidate, bytes);
    }

    // 5. Write the zip.
    let total_assets: usize = assets.values().map(BTreeMap::len).sum();
    Bundle::create(
        out_manifest,
        &wasms,
        &screenshot_bytes,
        &assets,
        args.out.as_std_path(),
    )
    .context("failed to create bundle")?;

    info!(
        "Bundle written to {} ({} canister(s), {} screenshot(s), {} asset file(s))",
        args.out,
        canister_names.len(),
        build.screenshots.len(),
        total_assets,
    );

    Ok(())
}

// --- Helpers ---------------------------------------------------------------

fn to_canister_arg(spec: &BuildCanisterArg) -> CanisterArg {
    CanisterArg {
        arg: spec.arg.clone(),
        format: spec.format.map(ArgFormat::from).unwrap_or_default(),
    }
}

/// Resolve a path that appeared in the build manifest. Absolute paths are
/// kept as-is; relative ones are joined with the manifest's directory.
fn resolve_relative(manifest_dir: &Path, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        manifest_dir.join(p)
    }
}

/// Recursively walk `dir` and return a map of `<relative_path> -> <bytes>`.
/// Paths use forward slashes regardless of host OS.
fn collect_asset_dir(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, anyhow::Error> {
    if !dir.exists() {
        return Err(anyhow!("asset directory '{dir}' does not exist"));
    }
    if !dir.is_dir() {
        return Err(anyhow!("asset path '{dir}' is not a directory"));
    }

    let mut out: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let iter = std::fs::read_dir(current.as_std_path())
            .with_context(|| format!("failed to read directory '{current}'"))?;
        for entry in iter {
            let entry = entry.with_context(|| format!("failed to read entry in '{current}'"))?;
            let path = PathBuf::from_path_buf(entry.path())
                .map_err(|p| anyhow!("non-utf8 path in asset dir: {}", p.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat '{path}'"))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .map_err(|e| anyhow!("failed to compute relative path for '{path}': {e}"))?;
            let rel_str = rel.as_str().replace('\\', "/");
            let bytes = std::fs::read(path.as_std_path())
                .with_context(|| format!("failed to read asset file '{path}'"))?;
            out.insert(rel_str, bytes);
        }
    }
    Ok(out)
}

/// Very small MIME-type guesser for screenshots. Only covers formats
/// users will reasonably drop in here.
fn guess_mime_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => return None,
        }
        .to_string(),
    )
}
