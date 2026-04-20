use std::collections::BTreeMap;

use anyhow::{Context as _, anyhow};
use clap::Args;
use icp::InitArgs;
use icp::context::{Context, EnvironmentSelection};
use icp::manifest::ArgsFormat;
use icp::prelude::PathBuf;
use icp_packaging::{
    ArgFormat, Bundle, CanisterArg, CanisterEntry, CanisterKind, Manifest, Screenshot,
    bundle::default_wasm_path,
};
use tracing::info;

use crate::options::EnvironmentOpt;

/// Create an application bundle (.icp-app zip) from previously built canisters.
///
/// The canister wasms are pulled from the local build artifact store, which
/// is populated by `icp build`. Run `icp build` for the same environment
/// before running this command.
#[derive(Debug, Args)]
pub(crate) struct CreateArgs {
    /// Human readable application name written into the manifest.
    #[arg(long)]
    pub(crate) name: String,

    /// Optional application version string, written into the manifest.
    #[arg(long)]
    pub(crate) application_version: Option<String>,

    /// Optional application description, written into the manifest.
    #[arg(long)]
    pub(crate) description: Option<String>,

    /// Output file to write the bundle to.
    #[arg(long, short = 'o')]
    pub(crate) out: PathBuf,

    /// Upgrade argument for a specific canister, in the form
    /// `CANISTER=CANDID_TEXT`. May be specified multiple times.
    ///
    /// Example: `--upgrade-arg backend='(opt variant { Upgrade })'`
    #[arg(long = "upgrade-arg", value_parser = parse_kv)]
    pub(crate) upgrade_args: Vec<(String, String)>,

    /// Declare that a canister depends on one or more other canisters, in
    /// the form `CANISTER=DEP1,DEP2`. May be specified multiple times.
    ///
    /// Example: `--depends frontend=backend`
    #[arg(long = "depends", value_parser = parse_kv)]
    pub(crate) depends: Vec<(String, String)>,

    /// Set an environment variable on a specific canister. Form:
    /// `CANISTER=KEY=VALUE`. May be specified multiple times.
    ///
    /// If VALUE is omitted (i.e. `CANISTER=KEY=`), the variable is written
    /// to the manifest as `null`, signalling to the installer that it must
    /// prompt the user for a value at install time.
    ///
    /// The installer will also automatically inject `CANISTER_ID_<name>`
    /// variables for every canister in the bundle, so those do not need to
    /// be specified here.
    ///
    /// Example: `--env backend=LOG_LEVEL=debug`
    /// Example: `--env backend=API_KEY=`   (prompted at install time)
    #[arg(long = "env", value_parser = parse_env_triple)]
    pub(crate) env: Vec<(String, String, Option<String>)>,

    /// Attach a screenshot to the bundle.
    ///
    /// Form: `PATH[,form_factor=narrow|wide][,label=TEXT][,sizes=WxH]`.
    /// The file at `PATH` on disk is read and packed under `screenshots/`
    /// in the zip, and a matching entry is added to the manifest.
    ///
    /// Example: `--screenshot ./shots/hero.png,form_factor=wide,label=Home`
    #[arg(long = "screenshot", value_parser = parse_screenshot_spec)]
    pub(crate) screenshots: Vec<ScreenshotSpec>,

    /// Canister names to include. If empty, all canisters in the selected
    /// environment are included.
    pub(crate) canisters: Vec<String>,

    #[command(flatten)]
    pub(crate) environment: EnvironmentOpt,
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| "expected KEY=VALUE".to_string())?;
    if k.is_empty() {
        return Err("KEY must not be empty".to_string());
    }
    Ok((k.to_string(), v.to_string()))
}

/// Parses `CANISTER=KEY=VALUE` (or `CANISTER=KEY=` for a null value).
fn parse_env_triple(s: &str) -> Result<(String, String, Option<String>), String> {
    let (canister, rest) = s
        .split_once('=')
        .ok_or_else(|| "expected CANISTER=KEY=VALUE".to_string())?;
    if canister.is_empty() {
        return Err("CANISTER must not be empty".to_string());
    }
    let (key, value) = rest
        .split_once('=')
        .ok_or_else(|| "expected CANISTER=KEY=VALUE".to_string())?;
    if key.is_empty() {
        return Err("KEY must not be empty".to_string());
    }
    let value = if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    };
    Ok((canister.to_string(), key.to_string(), value))
}

/// A CLI-level description of a screenshot to attach. Resolved at exec
/// time into a [`Screenshot`] manifest entry plus the file bytes.
#[derive(Clone, Debug)]
pub(crate) struct ScreenshotSpec {
    /// Source path on disk.
    pub(crate) path: PathBuf,
    /// Optional "narrow" / "wide" form factor.
    pub(crate) form_factor: Option<String>,
    /// Optional label.
    pub(crate) label: Option<String>,
    /// Optional sizes descriptor.
    pub(crate) sizes: Option<String>,
}

fn parse_screenshot_spec(s: &str) -> Result<ScreenshotSpec, String> {
    // PATH[,k=v]*
    let mut parts = s.split(',');
    let path = parts
        .next()
        .ok_or_else(|| "empty screenshot spec".to_string())?
        .trim();
    if path.is_empty() {
        return Err("screenshot PATH must not be empty".to_string());
    }
    let mut spec = ScreenshotSpec {
        path: PathBuf::from(path),
        form_factor: None,
        label: None,
        sizes: None,
    };
    for part in parts {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("expected key=value in screenshot spec, got '{part}'"))?;
        let k = k.trim();
        let v = v.trim();
        match k {
            "form_factor" => {
                if v != "narrow" && v != "wide" {
                    return Err(format!(
                        "form_factor must be 'narrow' or 'wide', got '{v}'"
                    ));
                }
                spec.form_factor = Some(v.to_string());
            }
            "label" => spec.label = Some(v.to_string()),
            "sizes" => spec.sizes = Some(v.to_string()),
            other => {
                return Err(format!(
                    "unknown key '{other}' in screenshot spec (expected form_factor, label, sizes)"
                ));
            }
        }
    }
    Ok(spec)
}

pub(crate) async fn exec(ctx: &Context, args: &CreateArgs) -> Result<(), anyhow::Error> {
    let environment_selection: EnvironmentSelection = args.environment.clone().into();
    let env = ctx.get_environment(&environment_selection).await?;

    // Decide which canisters to include.
    let canister_names: Vec<String> = if args.canisters.is_empty() {
        env.canisters.keys().cloned().collect()
    } else {
        args.canisters.clone()
    };

    if canister_names.is_empty() {
        return Err(anyhow!(
            "no canisters to package: environment '{}' has no canisters",
            env.name
        ));
    }

    // Validate that every canister name in args is actually known.
    for name in &canister_names {
        if !env.canisters.contains_key(name) {
            return Err(anyhow!(
                "canister '{name}' is not part of environment '{}'",
                env.name
            ));
        }
    }

    // Index --upgrade-arg and --depends flags by canister name.
    let mut upgrade_args: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &args.upgrade_args {
        upgrade_args.insert(k.clone(), v.clone());
    }
    let mut depends: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in &args.depends {
        let list: Vec<String> = v
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();
        depends.insert(k.clone(), list);
    }

    // Index --env flags by canister name. Each canister ends up with a
    // map of key -> Option<String> (None means "installer must prompt").
    let mut env_vars: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();
    for (canister, key, value) in &args.env {
        env_vars
            .entry(canister.clone())
            .or_default()
            .insert(key.clone(), value.clone());
    }

    // Build the manifest + wasm payload.
    let mut manifest = Manifest::new(&args.name);
    manifest.application_version = args.application_version.clone();
    manifest.description = args.description.clone();

    let mut wasms: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for name in &canister_names {
        let (_path, canister) = env
            .get_canister_info(name)
            .map_err(|e| anyhow!("failed to resolve canister info: {e}"))?;

        // Pull the built wasm out of the artifact store.
        let wasm = ctx
            .artifacts
            .lookup(name)
            .await
            .with_context(|| format!("no build artifact for canister '{name}'; run `icp build` first"))?;

        // Translate the per-canister init_args (if any) into a
        // CanisterArg. We only support text args (candid or json-as-candid)
        // — binary is not representable in the manifest format.
        let init_arg = canister
            .init_args
            .as_ref()
            .and_then(translate_init_arg);

        // Translate --upgrade-arg (assumed candid text).
        let upgrade_arg = upgrade_args.get(name).map(|s| CanisterArg {
            arg: s.clone(),
            format: ArgFormat::Candid,
        });

        // Collect dependencies for this canister.
        let dependencies = depends.get(name).cloned().unwrap_or_default();

        // Validate all declared dependencies are part of the bundle.
        for dep in &dependencies {
            if !canister_names.contains(dep) {
                return Err(anyhow!(
                    "canister '{name}' depends on '{dep}' which is not included in the bundle"
                ));
            }
        }

        // Collect env vars for this canister.
        let env_variables = env_vars.get(name).cloned().unwrap_or_default();

        let entry = CanisterEntry {
            kind: CanisterKind::Backend,
            path: default_wasm_path(name),
            dependencies,
            env_variables,
            init_arg,
            upgrade_arg,
        };

        manifest.canisters.insert(name.clone(), entry);
        wasms.insert(name.clone(), wasm);
    }

    // Reject unknown --upgrade-arg / --depends / --env targets.
    for name in upgrade_args.keys() {
        if !manifest.canisters.contains_key(name) {
            return Err(anyhow!(
                "--upgrade-arg for unknown canister '{name}' (not in the bundle)"
            ));
        }
    }
    for name in depends.keys() {
        if !manifest.canisters.contains_key(name) {
            return Err(anyhow!(
                "--depends for unknown canister '{name}' (not in the bundle)"
            ));
        }
    }
    for name in env_vars.keys() {
        if !manifest.canisters.contains_key(name) {
            return Err(anyhow!(
                "--env for unknown canister '{name}' (not in the bundle)"
            ));
        }
    }

    // Read and attach screenshots. Each file on disk is placed under
    // `screenshots/<basename>` inside the zip. If multiple screenshots
    // share the same basename we disambiguate by appending an index.
    let mut screenshot_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut used_names: std::collections::BTreeSet<String> = Default::default();
    for spec in &args.screenshots {
        let bytes = std::fs::read(spec.path.as_std_path())
            .with_context(|| format!("failed to read screenshot '{}'", spec.path))?;

        let file_name = spec
            .path
            .file_name()
            .ok_or_else(|| anyhow!("screenshot path '{}' has no file name", spec.path))?;
        let mut candidate = format!("screenshots/{file_name}");
        let mut n: u32 = 1;
        while used_names.contains(&candidate) {
            n += 1;
            candidate = format!("screenshots/{n}-{file_name}");
        }
        used_names.insert(candidate.clone());

        let form_factor = spec
            .form_factor
            .as_deref()
            .map(|v| match v {
                "narrow" => icp_packaging::ScreenshotFormFactor::Narrow,
                "wide" => icp_packaging::ScreenshotFormFactor::Wide,
                _ => unreachable!("validated by parse_screenshot_spec"),
            });

        let mime_type = guess_mime_type(&spec.path);

        manifest.screenshots.push(Screenshot {
            src: candidate.clone(),
            sizes: spec.sizes.clone(),
            mime_type,
            form_factor,
            label: spec.label.clone(),
        });
        screenshot_bytes.insert(candidate, bytes);
    }

    // Write the zip.
    Bundle::create(
        manifest,
        &wasms,
        &screenshot_bytes,
        args.out.as_std_path(),
    )
    .context("failed to create bundle")?;

    info!(
        "Bundle written to {} ({} canister(s), {} screenshot(s))",
        args.out,
        canister_names.len(),
        args.screenshots.len(),
    );

    Ok(())
}

/// Very small MIME-type guesser for screenshots. We only need to cover the
/// handful of formats users will reasonably drop in here.
fn guess_mime_type(path: &icp::prelude::Path) -> Option<String> {
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

/// Map `icp::InitArgs` to a `CanisterArg` suitable for the manifest file.
///
/// `InitArgs::Binary` is intentionally skipped: the manifest format stores
/// args as text only. This mirrors the MVP scope — binary init args are a
/// rare case and can be added later by encoding as hex or handling them
/// outside the bundle.
fn translate_init_arg(args: &InitArgs) -> Option<CanisterArg> {
    match args {
        InitArgs::Text { content, format } => {
            let format = match format {
                ArgsFormat::Candid => ArgFormat::Candid,
                // No json format in the current InitArgs surface; treat
                // hex and bin as candid text (best effort) or drop.
                ArgsFormat::Hex | ArgsFormat::Bin => return None,
            };
            Some(CanisterArg {
                arg: content.clone(),
                format,
            })
        }
        InitArgs::Binary(_) => None,
    }
}
