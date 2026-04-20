//! Bundle creation and consumption.
//!
//! A "bundle" is a zip file on disk with the following layout:
//!
//! ```text
//! manifest.json
//! canisters/<canister_name>.wasm.gz
//! ```
//!
//! The zip is written with DEFLATE compression and no encryption, per the
//! design doc. Each wasm is gzipped before being added to the archive; this
//! is a bit redundant with zip's own DEFLATE but matches the specification
//! (entries end in `.wasm.gz` and must be gzip-compressed, so an installer
//! that pulls the raw entry bytes gets a valid gzip stream).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use snafu::{ResultExt, Snafu};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::manifest::{CanisterEntry, Manifest};

/// Name of the manifest file inside the bundle.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Directory prefix for canister wasm entries inside the bundle.
pub const CANISTERS_DIR: &str = "canisters";

/// Errors that can occur while building a bundle.
#[derive(Debug, Snafu)]
pub enum CreateError {
    #[snafu(display("failed to create bundle file '{}'", path.display()))]
    OpenOutput {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("failed to serialize manifest to JSON"))]
    SerializeManifest { source: serde_json::Error },

    #[snafu(display("failed to write zip entry '{entry}'"))]
    WriteEntry {
        source: zip::result::ZipError,
        entry: String,
    },

    #[snafu(display("failed to write bytes for entry '{entry}'"))]
    WriteBytes {
        source: std::io::Error,
        entry: String,
    },

    #[snafu(display("failed to gzip-compress wasm for canister '{canister}'"))]
    GzipCompress {
        source: std::io::Error,
        canister: String,
    },

    #[snafu(display("failed to finish zip archive"))]
    FinishArchive { source: zip::result::ZipError },

    #[snafu(display(
        "canister '{canister}' is referenced in the manifest but no wasm was provided"
    ))]
    MissingWasm { canister: String },

    #[snafu(display("wasm bytes were provided for '{canister}' but it is not in the manifest"))]
    UnknownCanister { canister: String },

    #[snafu(display(
        "screenshot '{src}' is referenced in the manifest but no bytes were provided"
    ))]
    MissingScreenshot { src: String },

    #[snafu(display(
        "screenshot bytes were provided for '{src}' but it is not referenced in the manifest"
    ))]
    UnknownScreenshot { src: String },

    #[snafu(display("screenshot src '{src}' must not be empty"))]
    EmptyScreenshotSrc { src: String },
}

/// Errors that can occur while reading a bundle.
#[derive(Debug, Snafu)]
pub enum OpenError {
    #[snafu(display("failed to open bundle file '{}'", path.display()))]
    OpenInput {
        source: std::io::Error,
        path: PathBuf,
    },

    #[snafu(display("failed to read bundle as zip"))]
    ReadArchive { source: zip::result::ZipError },

    #[snafu(display("bundle is missing required entry '{MANIFEST_FILE}'"))]
    MissingManifest,

    #[snafu(display("failed to read entry '{entry}'"))]
    ReadEntry {
        source: zip::result::ZipError,
        entry: String,
    },

    #[snafu(display("failed to read bytes for entry '{entry}'"))]
    ReadBytes {
        source: std::io::Error,
        entry: String,
    },

    #[snafu(display("failed to parse manifest JSON"))]
    ParseManifest { source: serde_json::Error },

    #[snafu(display("failed to gunzip wasm for canister '{canister}'"))]
    GzipDecompress {
        source: std::io::Error,
        canister: String,
    },

    #[snafu(display(
        "canister '{canister}' declares dependency '{dep}' which is not in the manifest"
    ))]
    UnknownDependency { canister: String, dep: String },

    #[snafu(display("manifest has a dependency cycle involving '{canister}'"))]
    DependencyCycle { canister: String },
}

/// In-memory representation of an opened bundle.
///
/// When a bundle is opened via [`Bundle::open`] we eagerly load every wasm
/// and screenshot into memory. Bundles are expected to be small-ish (a
/// handful of wasms each a few MB, plus a few screenshots) so this keeps
/// the API ergonomic for the MVP.
#[derive(Debug)]
pub struct Bundle {
    pub manifest: Manifest,
    /// Decompressed wasm bytes keyed by canister name.
    pub wasms: BTreeMap<String, Vec<u8>>,
    /// Screenshot bytes keyed by the `src` path from the manifest (which is
    /// also the path inside the zip). Empty if the manifest declares no
    /// screenshots.
    pub screenshots: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    /// Create a new bundle on disk.
    ///
    /// - `manifest` describes the application and its canisters. The caller
    ///   is responsible for populating the [`CanisterEntry::path`] fields;
    ///   if they are empty we default them to `<name>.wasm.gz`.
    /// - `wasms` maps canister name to the *raw* (uncompressed) wasm bytes.
    ///   Keys must match exactly the canister names in `manifest.canisters`.
    /// - `screenshots` maps the `src` path recorded in each
    ///   [`crate::manifest::Screenshot`] to the raw image bytes. Keys must
    ///   match exactly the `src` values in `manifest.screenshots`. Pass an
    ///   empty map if the manifest declares no screenshots.
    /// - `out` is the path to the zip file to produce. Any existing file at
    ///   that path will be overwritten.
    pub fn create(
        mut manifest: Manifest,
        wasms: &BTreeMap<String, Vec<u8>>,
        screenshots: &BTreeMap<String, Vec<u8>>,
        out: &Path,
    ) -> Result<(), CreateError> {
        // Basic sanity: every canister in the manifest has wasm, and vice versa.
        for name in manifest.canisters.keys() {
            if !wasms.contains_key(name) {
                return MissingWasmSnafu {
                    canister: name.clone(),
                }
                .fail();
            }
        }
        for name in wasms.keys() {
            if !manifest.canisters.contains_key(name) {
                return UnknownCanisterSnafu {
                    canister: name.clone(),
                }
                .fail();
            }
        }

        // Same sanity check for screenshots.
        for shot in &manifest.screenshots {
            if shot.src.is_empty() {
                return EmptyScreenshotSrcSnafu {
                    src: shot.src.clone(),
                }
                .fail();
            }
            if !screenshots.contains_key(&shot.src) {
                return MissingScreenshotSnafu {
                    src: shot.src.clone(),
                }
                .fail();
            }
        }
        for src in screenshots.keys() {
            if !manifest.screenshots.iter().any(|s| &s.src == src) {
                return UnknownScreenshotSnafu { src: src.clone() }.fail();
            }
        }

        // Default the `path` field if not set by the caller.
        for (name, entry) in manifest.canisters.iter_mut() {
            if entry.path.is_empty() {
                entry.path = default_wasm_path(name);
            }
        }

        let file = File::create(out).context(OpenOutputSnafu {
            path: out.to_path_buf(),
        })?;
        let mut zip = ZipWriter::new(file);

        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        // manifest.json
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).context(SerializeManifestSnafu)?;
        zip.start_file(MANIFEST_FILE, options)
            .context(WriteEntrySnafu {
                entry: MANIFEST_FILE.to_string(),
            })?;
        zip.write_all(&manifest_bytes).context(WriteBytesSnafu {
            entry: MANIFEST_FILE.to_string(),
        })?;

        // canisters/<path>
        for (name, entry) in &manifest.canisters {
            let wasm = wasms.get(name).expect("presence checked above");
            let gzipped = gzip_bytes(wasm).context(GzipCompressSnafu {
                canister: name.clone(),
            })?;

            let zip_entry_name = format!("{CANISTERS_DIR}/{}", entry.path);
            zip.start_file(&zip_entry_name, options)
                .context(WriteEntrySnafu {
                    entry: zip_entry_name.clone(),
                })?;
            zip.write_all(&gzipped).context(WriteBytesSnafu {
                entry: zip_entry_name,
            })?;
        }

        // screenshots. The `src` is already a relative path inside the zip,
        // so we use it verbatim.
        for shot in &manifest.screenshots {
            let bytes = screenshots.get(&shot.src).expect("presence checked above");
            zip.start_file(&shot.src, options)
                .context(WriteEntrySnafu {
                    entry: shot.src.clone(),
                })?;
            zip.write_all(bytes).context(WriteBytesSnafu {
                entry: shot.src.clone(),
            })?;
        }

        zip.finish().context(FinishArchiveSnafu)?;
        Ok(())
    }

    /// Open an existing bundle from a file on disk.
    ///
    /// Reads the manifest, decompresses every canister's wasm, and performs
    /// a small amount of structural validation:
    ///   - every declared dependency refers to a canister in the manifest
    ///   - the dependency graph has no cycles
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let file = File::open(path).context(OpenInputSnafu {
            path: path.to_path_buf(),
        })?;
        Self::open_reader(file)
    }

    /// Open an existing bundle from in-memory bytes.
    ///
    /// Useful on the consumer side when the bundle arrives as a byte blob,
    /// e.g. as multipart form data in an HTTP request body.
    pub fn open_bytes(bytes: &[u8]) -> Result<Self, OpenError> {
        Self::open_reader(std::io::Cursor::new(bytes.to_vec()))
    }

    fn open_reader<R: Read + std::io::Seek>(reader: R) -> Result<Self, OpenError> {
        let mut archive = ZipArchive::new(reader).context(ReadArchiveSnafu)?;

        // Read manifest.
        let manifest: Manifest = {
            let mut entry = match archive.by_name(MANIFEST_FILE) {
                Ok(e) => e,
                Err(zip::result::ZipError::FileNotFound) => {
                    return MissingManifestSnafu.fail();
                }
                Err(e) => {
                    return Err(e).context(ReadEntrySnafu {
                        entry: MANIFEST_FILE.to_string(),
                    });
                }
            };
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).context(ReadBytesSnafu {
                entry: MANIFEST_FILE.to_string(),
            })?;
            serde_json::from_slice(&buf).context(ParseManifestSnafu)?
        };

        validate_dependencies(&manifest)?;

        // Read and decompress each canister wasm.
        let mut wasms: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (name, entry) in &manifest.canisters {
            let zip_entry_name = format!("{CANISTERS_DIR}/{}", entry.path);
            let mut zentry = archive.by_name(&zip_entry_name).context(ReadEntrySnafu {
                entry: zip_entry_name.clone(),
            })?;
            let mut gz_bytes = Vec::new();
            zentry.read_to_end(&mut gz_bytes).context(ReadBytesSnafu {
                entry: zip_entry_name,
            })?;
            let wasm = gunzip_bytes(&gz_bytes).context(GzipDecompressSnafu {
                canister: name.clone(),
            })?;
            wasms.insert(name.clone(), wasm);
        }

        // Read screenshot blobs. Screenshots are stored verbatim, so we
        // just pull the raw bytes at the `src` path.
        let mut screenshots: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for shot in &manifest.screenshots {
            let mut zentry = archive.by_name(&shot.src).context(ReadEntrySnafu {
                entry: shot.src.clone(),
            })?;
            let mut buf = Vec::new();
            zentry.read_to_end(&mut buf).context(ReadBytesSnafu {
                entry: shot.src.clone(),
            })?;
            screenshots.insert(shot.src.clone(), buf);
        }

        Ok(Bundle {
            manifest,
            wasms,
            screenshots,
        })
    }
}

/// Default path stored in the manifest for a given canister name.
pub fn default_wasm_path(name: &str) -> String {
    format!("{name}.wasm.gz")
}

fn gzip_bytes(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

fn gunzip_bytes(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

/// Minimal validation of the `dependencies` graph:
///   - every referenced name must exist in the manifest
///   - no cycles
///
/// This is intentionally small; a real installer will re-run a proper
/// topological sort when deciding install order.
fn validate_dependencies(manifest: &Manifest) -> Result<(), OpenError> {
    // First: all deps reference known canisters.
    for (name, entry) in &manifest.canisters {
        for dep in &entry.dependencies {
            if !manifest.canisters.contains_key(dep) {
                return UnknownDependencySnafu {
                    canister: name.clone(),
                    dep: dep.clone(),
                }
                .fail();
            }
        }
    }

    // Second: cycle check via DFS.
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        InStack,
        Done,
    }

    fn dfs<'a>(
        node: &'a str,
        canisters: &'a BTreeMap<String, CanisterEntry>,
        marks: &mut BTreeMap<&'a str, Mark>,
    ) -> Result<(), OpenError> {
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
        marks.insert(node, Mark::InStack);
        if let Some(entry) = canisters.get(node) {
            for dep in &entry.dependencies {
                dfs(dep.as_str(), canisters, marks)?;
            }
        }
        marks.insert(node, Mark::Done);
        Ok(())
    }

    let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
    for name in manifest.canisters.keys() {
        dfs(name.as_str(), &manifest.canisters, &mut marks)?;
    }

    Ok(())
}
