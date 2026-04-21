# Package manifest (build input)

`icp package create --manifest <FILE>` reads a JSON file describing the
bundle to build. This is the **build manifest** — it mirrors the runtime
`manifest.json` that ends up inside the zip, with a few extra fields that
only make sense at build time.

All file paths in the build manifest are resolved **relative to the
manifest file's own directory** (not the shell's working directory), so
the manifest can be checked in next to the files it references.

## Minimal example

```json
{
  "name": "ICDocs",
  "application_version": "0.1.0",
  "description": "Decentralized document management on the Internet Computer",
  "canisters": {
    "backend": {
      "dependencies": []
    },
    "frontend": {
      "dependencies": ["backend"],
      "asset_dir": "./frontend/dist"
    }
  },
  "screenshots": [
    { "src": "./frontend/public/logo.png", "form_factor": "wide", "label": "ICDocs logo" }
  ]
}
```

With that file saved as `./package.json`, the build command becomes:

```bash
icp build -e staging
icp package create --manifest ./package.json --out ./icdocs.icp-app -e staging
```

## Build-time-only fields

These appear in the build manifest but are stripped or transformed before
the bundle is written:

| Field | Location | Behavior |
|---|---|---|
| `canisters.<name>.wasm` | per-canister, optional | Path to a wasm file on disk. When omitted, the wasm is looked up in the local artifact store (populated by `icp build`, selected with `-e <environment>`). |
| `canisters.<name>.asset_dir` | per-canister, optional | Path to a directory whose contents are recursively packed under `assets/<name>/` in the zip. Presence of this field implies `type: assets`. |
| `screenshots[].src` | per-screenshot | Path to an image file on disk. The builder reads the bytes and rewrites `src` to `screenshots/<basename>` inside the zip. |

## Runtime-manifest fields (passed through verbatim)

See `packaging_design.md` at the repo root for the full schema. Fields
currently honored by the builder:

- `name` (required)
- `short_name` — optional shorter form of `name` for space-constrained surfaces.
- `application_version`
- `description`
- `manifest_version` (defaults to `1`)
- `canisters.<name>.type` — `backend` (default) or `assets`; overridden to `assets` when `asset_dir` is set.
- `canisters.<name>.dependencies` — list of other canister names in this bundle.
- `canisters.<name>.env_variables` — map of `key -> string | null`. A `null` value means "installer must prompt the user at install time".
- `canisters.<name>.init_arg` — `{ "arg": "(...)", "format": "candid" | "json" }`.
- `canisters.<name>.upgrade_arg` — same shape as `init_arg`.
- `screenshots[]` — `{ src, sizes?, type?, form_factor?, label? }` (`src` interpreted per above).

## Full example

```json
{
  "manifest_version": 1,
  "name": "ICDocs",
  "application_version": "0.1.0",
  "description": "Decentralized document management on the Internet Computer",

  "canisters": {
    "backend": {
      "init_arg": { "arg": "()", "format": "candid" },
      "env_variables": {
        "LOG_LEVEL": "info",
        "API_KEY": null
      }
    },
    "frontend": {
      "dependencies": ["backend"],
      "asset_dir": "./frontend/dist"
    }
  },

  "screenshots": [
    {
      "src": "./frontend/public/logo.png",
      "form_factor": "wide",
      "label": "ICDocs logo"
    }
  ]
}
```

## Behavior notes

- **Unknown top-level or per-canister keys are rejected.** Typos surface
  as parse errors rather than being silently ignored.
- **Empty canister list is an error.** The manifest must declare at least
  one canister.
- **Dependencies are validated.** Every name in `canisters.<name>.dependencies`
  must also appear as a canister key in the same manifest.
- **Wasm resolution.** If *any* canister omits `wasm`, the artifact store
  for the selected environment is loaded. If no environment is specified,
  `-e local` is assumed.
- **Env var `null` values** stay `null` in the final bundle — they signal
  to the consumer/installer that it must prompt the user for a value.
  Synthetic variables (`__CANISTER_NAME`, `__CANISTER_DESCRIPTION`,
  `__CANISTER_PROJECT`, `<DEP>_CANISTER_ID`) are injected by the consumer
  at install time and don't need to be listed here.
