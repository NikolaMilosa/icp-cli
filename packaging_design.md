# ICP Packaged Application Format

# **Background**

## **Glossary**

Application \- A collection of canisters, assets and configuration.
Builder \- The software that generates the ICP application file.
Installer \- The software that installs the application.

## **Introduction**

Currently, applications on the ICP are primarily built and deployed using `icp`. The distribution of these applications is challenging. To share an application, one must send all the canisters and any required frontend assets. Recipients then need specific knowledge to install the application, including:

1. Which canister the assets should be pushed to.  
2. What initialization arguments to specify.  
3. What environment variables should be set.  
4. What canister settings should be used (e.g., controllers).

This lack of a standardized application format hinders the creation of an application marketplace. A defined application file format is needed to package multiple canisters, their assets, settings, and installation instructions together for easy distribution and installation.

While UTOPIA's needs served as the initial catalyst for this proposal, the benefits extend to other projects like Caffeine and ICP Ninja, and potentially the broader IC ecosystem. We recognize the value of standardization and aim to transition this proposal into a community-driven standard in the medium term.

# **Objective**

## **Goals**

The Application format has to fulfill the following requirements:

1. Allow installation and updates of an application after it has been built, without requiring the original source code or development environment.  
2. Be easily distributable as a single, self-contained file.  
3. Contain all of the application’s components and information needed for installation, including canister configurations, assets, and installation instructions.  
4. Allow for reproducibility from source code, if the source code produces reproducible canisters and assets.

In addition, the document should describe how this file should be created and used to install an application.

# **Use cases**

## **Marketplace**

A marketplace is a central location that distributes applications created by developers. The marketplace would be performing application installation for the user. Developers would upload their applications to the marketplace using the application format defined here.

## **Caffeine**

Similarly to the ICP Ninja case, caffeine users will be able to publish directly to a marketplace and monetize their applications.

# **Proposal**

The following proposal draws inspiration from the way APK files are packaged in Android. 

The application must be packaged as a zip file. The zip file may use compression. The zip file must not use encryption.

The zip file must have the following contents:

1. manifest.json  
2. canisters/

Optionally, the zip file may have the assets folder:

3. assets/

## **The manifest file**

The manifest file is a json file describing the application and its content. It contains an entry for every canister in the application. Entries are similar in their structure to the canister entries in dfx.json, but contain only the information required for creating and installing canisters. Additional fields are:

1. `name` \- A user-facing name of the application.  
2. `short_name` \- The name of the application for space-constrained environments. (optional).  
3. `application_version` \- The version of the application. (optional).  
4. `author` \- Author of the application. (optional).  
5. `description` \- Text describing the application. (optional).  
6. `icons` \- A list of icon files that represent the application. (optional).  
7. `screenshots` \- A list of screenshots from the application. (optional).  
8. `categories` \- A list of categories the application belongs to. (optional).   
9. `license` \- The application’s license. (optional)  
10. `source_link` \- A link to the application’s source code. (optional).  
11. `main_canister` \- The name of the Application’s main canister. (optional).  
12. `canisters.[canister].env_variables` which specifies a mapping of environment variables and their values that should be set in the canister during installation. A value of null indicates that the Installer should be the one setting the value for this environment variable.  (optional).  
13. `canisters.[canister].controllers` in the canister’s config \- specifies a list of canister names that should be controllers in addition to the installer. These must be canister names defined in the manifest’s `canisters` list.  (optional).  
14. `canisters.[canister].upgrade_arg` in the canister’s config \- Contains a candid or json string with the upgrade arg that should be used if the Application is being upgraded, also contains the argument format (canidid/json). If candid is used and a definitions file path is provided then the service init arg type will be used when encoding this member. (optional).  
15. `canisters.[canister].candid_definitions_path` \- A file with the candid types which are used when encoding `init_arg` and `update_arg`. (optional).  

The full manifest file schema is attached in [appendix 1](#appendix-1---manifest-schema).

## **The canisters/ folder**

The application zip must contain in the `canisters/` folder a file for each canister in the manifest file. The file must have a `.wasm.gz` suffix, and be compressed with `gzip`.

Candid files for `init_arg` and `upgrade_arg` encoding should reside in this folder.

## **The assets/ folder**

This folder contains all the assets required for canisters of type `assets`. All assets must be stored in this folder. The Builder must use sub-folders to keep assets organized. Assets for an assets canister must be in a subfolder with the canister name.

# **Technical considerations**

## **Asset Canister & sdk integration**

The asset canister must serve the environment variables set by the Installer. The frontend code must be able to read those environment variables. For serving these environment variables the asset canister should either use a cookie as suggested in an earlier [document for loading frontend environment variables](https://docs.google.com/document/d/1SHZ5O3dGF8pXAGhhS3M38QG9203_o63SX7dJ4i4X1z4/edit?usp=sharing), or be served at a well known path by the asset canister, or both. The frontend sdk must be able to read those variables.

The installer must push the assets to the assets canister using the `identity` content encoding. The Installer may also compress the assets and upload to assets canister using `gzip` content encoding.

The installer must set the content type of the assets based on the file extension.

## **Building an Application & Reproducibility**

It is expected the manifest will not be crafted manually. It should be generated by the Builder based on a more user-friendly input. The exact specification of this input is out of scope for this document and may change from one Builder to another.

The builder should live in a separate crate so that it can be imported as rust crate in other projects wrapping the functionality or just fetching its internals to consume the zip.

## **Installing an application**

To collect all the environment variables before starting the canisters the Installer should first create all the canisters, then set the environment variables with the right canister ids.

The installer must follow this order of operations when installing an application:

1. Extract the manifest from the Application file.  
2. Check manifest file validity. Check that it is a valid json file, structured as expected, and no cycles in dependencies.  
3. Create all of the canisters in the manifest file. Canisters should be created with the amount of cycles specified in the manifest. Canisters are created with the Installer’s principal as a controller.  
4. Set additional controllers to the canisters in the Application to match those specified in the manifest.  
5. Set the environment variables of all canisters. The Installer must create an environment variable  `CANISTER_ID_x` where `x` is the canister name for every canister in the Application. The Installer will set all of these variables to all of the canisters. In addition the Installer must set the canister-specific variables defined in the manifest. If a variable has the value `null` in the manifest the Installer should prompt the user to provide a value for it.  
6. Install the code for all canisters, using `init_arg`. The order of installation for canisters after they were created should be a valid topological order for the graph defined by the dependencies.  
7. Push assets to assets canisters.  

## **Updating an application**

Updating an already installed Application requires the Installer to know details about the already installed Application \- the canister IDs and the manifest of the installed application. The way in which the Installer should keep track of this information is out of scope for this document.

The installer must follow this order of operations when updating an application:

1. Extract the manifest from the updated Application file.  
2. Check manifest file validity. Check that it is a valid json file, structured as expected, and no cycles in dependencies.  
3. Stop all of the modified and deleted canisters in the existing Application.  
4. Create new canisters which do not exist in the current version of the Application. New canisters should be created with the amount of cycles specified in the manifest.  
5. Set controllers to the canisters in the Application to match those specified in the manifest.  
6. Set the environment variables of all canisters in the updated Application to have the updated canister IDs. The Installer must create an environment variable  `CANISTER_ID_x` where `x` is the canister name for every canister in the Application. The Installer will set all of these variables to all of the canisters. In addition the Installer must set the canister-specific variables defined in the manifest. If a variable has the value `null` in the manifest the Installer should prompt the user to provide a value for it.  
7. Install the code for new and modified canisters, using the `upgrade_arg` for existing canisters and `init_arg` for new canisters. The order of installation for canisters after they were created should be a valid topological order for the graph defined by the dependencies.  
8. Push, replace, and delete assets from assets canisters.  

# **Examples**

## **Example 1 \- An application with an asset canister and backend**

For the common use case of an application with a single frontend and a single backend canister the manifest file could look like this:

```javascript
{
  "manifest_version": 1,
  "name": "Sample application",
  "application_version": "1.0.0",
  "canisters": {
    "backend": {
      "type": "backend",
      "path": "backend.wasm.gz",
      "env_variables": {
        "LOG_LEVEL": "debug"
      },
      "init_arg": {
        "arg": "()"
      }
    },
    "frontend": {
      "dependencies": [
        "backend"
      ],
      "path": "assets.wasm.gz",
      "initial_cycles": 500000000000,
      "type": "assets"
    }
  }
}
```

With a file structure such as:

```
manifest.json
canisters/backend.wasm.gz
canisters/assets.wasm.gz
assets/frontend/index.html
```

Installation should go as follows:

1. The backend canister will be created with the principal of the Installer as a controller.   
2. The assets canister will be created with the principal of the Installer as the controller.  
3. The backend canister will have its environment variables set, setting `CANISTER_ID_BACKEND` to itself, `CANISTER_ID_FRONTEND` to the frontend’s canister ID, and `LOG_LEVEL` to `debug`.  
4. The assets canister will have its environment variables set, setting `CANISTER_ID_BACKEND` to the backend canister’s principal, `CANISTER_ID_FRONTEND` to the frontend’s canister ID.  
5. Backend canister will be installed with the init arg of `()`. 
6. Frontend canister will be installed without supplying an init arg. 
7. The Installer will unpack the assets and push them to the assets canister.

## **Example 2 \- A self controlling Application with a init args**

If an application wants to have two canisters controlling each other, and get non-trivial init arguments 

```javascript
{
  "manifest_version": 1,
  "name": "Sample application 2",
  "application_version": "1.0.0",
  "canisters": {
    "backend1": {
      "type": "backend",
      "path": "backend1.wasm.gz",
      "candid_definitions_path": "backend1.did",
      "init_arg": {
        "arg": "(record { my_number = -500; my_string = \"aaa\"; })"
      }
      "controllers": [
        "backend2"
      ]
    },
    "backend2": {
      "type": "backend",
      "path": "backend2.wasm.gz",
      "controllers": [
        "backend1"
      ],
    }
  }
}
```

With a file structure such as:

```
manifest.json
canisters/backend1.wasm.gz
canisters/backend1.did
canisters/backend2.wasm.gz
```

Installation should go as follows:

1. The backend1 canister will be created with the principal of the Installer as a controller.   
2. The backend2 canister will be created with the principal of the Installer as the controller.  
3. backend1’s controllers will be updated to have backend2 added.  
4. backend2’s controller will be updated to have backend1 added.  
5. The init arg of backend1 will be encoded using the provided did file. 
6. backend2 canister will be installed without supplying an init arg.

## **Example 3 \- Application upgrade**

If an application is already installed with this manifest:

```javascript
{
  "manifest_version": 1,
  "name": "Sample application 3",
  "application_version": "1.0.0",
  "canisters": {
    "backend1": {
      "type": "backend",
      "path": "backend1.wasm.gz",
      "candid_definitions_path": "backend1.did",
      "init_arg": {
        "arg": "(opt variant { Init = record {init_arg = \"hi\"}})"
      }
      "controllers": [
        "backend2"
      ]
    },
    "backend2": {
      "type": "backend",
      "path": "backend2.wasm.gz",
      "controllers": [
        "backend1"
      ]
    }
  }
}
```

And the new version of the application has the manifest

```javascript
{
  "manifest_version": 1,
  "name": "Sample application 3",
  "application_version": "2.0.0",
  "canisters": {
    "backend1": {
      "type": "backend",
      "path": "backend1.wasm.gz",
      "candid_definitions_path": "backend1.did",
      "init_arg": {
        "arg": "(opt variant { Upgrade = record {upgrade_arg = \"hi\"}})"
      }
    },
    "backend3": {
      "type": "backend",
      "path": "backend3.wasm.gz",
      "controllers": [
        "backend1"
      ]
    }
  }
}
```

And the `backend1.did` file contains:

```
type ServiceArg = variant {
  // The configuration to use when initializing the canister.
  Init : SystemInit;
  // The configuration to use when upgrading the canister.
  Upgrade : SystemUpgrade;
};

type SystemInit = record {
    init_arg : text;
};

type SystemUpgrade = record {
    upgrade_arg : text;
};

```

Then the Application upgrade will follow this order of operations (after checking the signature validity, extracting the manifest and checking it is valid, and assuming backend1 has a different hash in the new version):

1. Canisters backend1 and backend2 will be stopped.   
2. The canister for backend3 will be created with the Installer as the controller.  
3. Controllers of backend1 will be changed to have only the Installer. Controllers of backend3 will be updated to include backend1.  
4.  Set the environment variables of backend1 and backend3 to have `CANISTER_ID_BACKEND1` and `CANISTER_ID_BACKEND3.`  
5. The Installer will upgrade backend1, providing the encoded upgrade\_arg using the provided candid definitions file.  
6. The installer will install backend3.  

# **Appendix 1 \- Manifest Schema** {#appendix-1---manifest-schema}

```javascript
{
    "$schema": "http://json-schema.org/draft-07/schema#",
    "title": "manifest.json",
    "type": "object",
    "required": [
      "manifest_version",
      "name",
      "canisters"
    ],
    "properties": {
      "name": {
        "title": "Name",
        "description": "The name of the application.",
        "type": "string"
      },
      "short_name": {
        "title": "Short Name",
        "description": "A short version of the application's name. Used in places where space is limited.",
        "type": "string"
      },
      "application_version": {
        "title": "Version",
        "description": "The version of the application.",
        "type": "string"
      },
      "author": {
        "title": "Author",
        "description": "The author of the application.",
        "type": "string"
      },
      "description": {
        "title": "Description",
        "description": "A description of the application.",
        "type": "string"
      },
      "icons": {
        "title": "Icons that represent the application",
        "description": "A list of icons that represent the application. The icons will be used in different contexts.",
        "type": [
          "array", 
          "null"
        ],
        "items": {
          "type": "object",
          "required": ["src"],
          "properties": {
            "src": {
              "type": "string",
              "description": "The path to the icon file from the root of the application."
            },
            "sizes": {
              "type": "string", 
              "description": "Space-separated list of icon dimensions (e.g. '48x48 72x72'). If not provided the size used is not guaranteed. For scalable icons such as SVGs, you can use 'any'."
            },
            "type": {
              "type": "string",
              "description": "MIME type of the icon (e.g. 'image/png'). If not provided the type used is deduced from the file extension."
            },
            "purpose": {
              "type": "string",
              "description": "Purpose of the icon - must be one of: 'any', 'maskable', or 'monochrome'.",
              "default": "any",
              "enum": ["any", "maskable", "monochrome"]
            }
          }
        }
      },
      "screenshots": {
        "title": "Screenshots",
        "description": "A list of screenshots of the application. Each screenshot object contains src, sizes, type, form_factor and optional label properties.",
        "type": [
          "array",
          "null"
        ],
        "items": {
          "type": "object",
          "required": ["src"],
          "properties": {
            "src": {
              "type": "string",
              "description": "The path to the screenshot image file."
            },
            "sizes": {
              "type": "string",
              "description": "The size of the screenshot in pixels, specified as a space-separated string (e.g. '1280x720'). If not provided the size used is not guaranteed. For scalable screenshots such as SVGs, you can use 'any'."
            },
            "type": {
              "type": "string", 
              "description": "The MIME type of the screenshot (e.g. 'image/png'). If not provided the type used is deduced from the file extension."
            },
            "form_factor": {
              "type": "string",
              "description": "The form factor the screenshot was taken on.",
              "enum": ["narrow", "wide"]
            },
            "label": {
              "type": "string",
              "description": "A short description of what the screenshot contains."
            }
          }
        }
      },
      "categories": {
        "title": "Categories",
        "description": "A list of categories the application belongs to. The categories should be from the list of w3c standard categories.",
        "type": [
          "array",
          "null"
        ],
        "items": {
          "type": "string"
        }
      },
      "source_link": {
        "title": "Source Link",
        "description": "A link to the source code of the application.",
        "type": "string"
      },
      "main_canister": {
        "title": "Main Canister",
        "description": "The main canister of the application. Must be a canister name mentioned in the manifest.",
        "type": [
          "string",
          "null"
        ],
        "default": null
      },
      "canisters": {
        "description": "Mapping between canisters and their settings.",
        "type": [
          "object",
          "null"
        ],
        "additionalProperties": {
          "$ref": "#/definitions/ConfigCanistersCanister"
        }
      },
      "manifest_version": {
        "title": "File format version",
        "description": "Pins the version of the ICP packaged application file format.",
        "type": [
          "integer"
        ],
        "format": "uint32",
        "minimum": 0
      }
    },
    "definitions": {
      "Byte": {
        "title": "Byte Count",
        "description": "A quantity of bytes. Representable either as an integer, or as an SI unit string",
        "examples": [72, "2KB",
          "4 MiB"
        ],
        "type": [
          "integer",
          "string"
        ],
        "pattern": "^[0-9]+( *([KkMmGgTtPpEeZzYy]i?)?[Bb])?$"
      },
      "CanisterLogVisibility": {
        "oneOf": [
          {
            "type": "string",
            "enum": [
              "controllers",
              "public"
            ]
          },
          {
            "type": "object",
            "required": [
              "allowed_viewers"
            ],
            "properties": {
              "allowed_viewers": {
                "type": "array",
                "items": {
                  "type": "string"
                }
              }
            },
            "additionalProperties": false
          }
        ]
      },
      "ConfigCanistersCanister": {
        "title": "Canister Configuration",
        "description": "Configurations for a single canister.",
        "type": "object",
        "oneOf": [
          {
            "title": "Backend-specific Properties",
            "type": "object",
            "required": [
              "type",
              "path"
            ],
            "properties": {
              "type": {
                "type": "string",
                "enum": [
                  "backend"
                ]
              }
            }
          },
          {
            "title": "Asset-Specific Properties",
            "type": "object",
            "required": [
              "path",
              "type"
            ],
            "properties": {
              "type": {
                "type": "string",
                "enum": [
                  "assets"
                ]
              }
            }
          }
        ],
        "properties": {
          "path": {
            "title": "Path",
            "description": "The path to the canister. Relative to the canisters folder.",
            "type": "string"
          },
          "dependencies": {
            "title": "Dependencies",
            "description": "Defines on which canisters this canister depends on.",
            "default": [],
            "type": "array",
            "items": {
              "type": "string"
            }
          },
          "env_variables": {
            "title": "Environment Variables",
            "description": "Environment variables for this canister.",
            "default": [],
            "type": "object",
            "additionalProperties": {
              "type": [
                "string",
                "null"
              ]
            }
          },
          "init_arg": {
            "title": "Init Arg", 
            "description": "The initialization argument used when installing the canister.",
            "anyOf": [
              {
                "$ref": "#/definitions/CanisterArg"
              },
              {
                "type": "null"
              }
            ],
            "default": null
          },
          "upgrade_arg": {
            "title": "Upgrade Arg",
            "description": "The argument used when upgrading the canister.",
            "anyOf": [
              {
                "$ref": "#/definitions/CanisterArg"
              },
              {
                "type": "null"
              }
            ],
            "default": null
          },
          "candid_definitions_path": {
            "title": "Candid Type definitions file path",
            "description": "A .did file path for the canister for correctly encoding the init and upgrade arguments. The file must be in the canisters folder. If provided, the serializer will use the service init arg type definition from this file for encoding both init_arg and upgrade_arg.",
            "type": [
              "string",
              "null"
            ]
          },
          "shortcuts": {
            "title": "Shortcuts",
            "description": "A list of shortcuts/links to key tasks or pages within the canister that can be used by browsers.",
            "type": ["array", "null"],
            "items": {
              "type": "object",
              "required": ["name", "url"],
              "properties": {
                "name": {
                  "type": "string",
                  "description": "The name of the shortcut as it is usually displayed to the user."
                },
                "short_name": {
                  "type": "string", 
                  "description": "A shorter version of the name, intended for tight spaces."
                },
                "description": {
                  "type": "string",
                  "description": "Description of what the shortcut does."
                },
                "url": {
                  "type": "string",
                  "description": "The URL that opens when using the shortcut."
                },
                "icons": {
                  "type": "array",
                  "description": "Icons representing the shortcut.",
                  "items": {
                    "type": "object",
                    "required": ["src"],
                    "properties": {
                      "src": {
                        "type": "string",
                        "description": "The path to the icon file from the root of the application."
                      },
                      "sizes": {
                        "type": "string",
                        "description": "The sizes of the icon, specified as a space-separated string of dimensions (e.g. '48x48 72x72')."
                      },
                      "type": {
                        "type": "string",
                        "description": "The MIME type of the icon (e.g. 'image/png')."
                      }
                    }
                  }
                }
              }
            }
          },
          "initialization_values": {
            "title": "Resource Allocation Settings",
            "description": "Defines initial values for resource allocation settings.",
            "default": {
              "compute_allocation": null,
              "freezing_threshold": null,
              "log_visibility": null,
              "memory_allocation": null,
              "reserved_cycles_limit": null,
              "wasm_memory_limit": null,
              "wasm_memory_threshold": null
            },
            "allOf": [
              {
                "$ref": "#/definitions/InitializationValues"
              }
            ]
          },
          "controllers": {
            "title": "Additional controllers",
            "description": "Canister names that should be set as controllers of this canister. These are in addition to the principal of the installer. The names must be canister names mentioned in the manifest.",
            "type": [
              "array"
            ],
            "items": {
              "type": "string"
            },
            "default": []
          }
        }
      },
      "InitializationValues": {
        "title": "Initial Resource Allocations",
        "type": "object",
        "properties": {
          "compute_allocation": {
            "title": "Compute Allocation",
            "description": "Must be a number between 0 and 100, inclusively. It indicates how much compute power should be guaranteed to this canister, expressed as a percentage of the maximum compute power that a single canister can allocate.",
            "default": null,
            "anyOf": [
              {
                "$ref": "#/definitions/PossiblyStr_for_uint64"
              },
              {
                "type": "null"
              }
            ]
          },
          "freezing_threshold": {
            "title": "Freezing Threshold",
            "description": "Freezing threshould of the canister, measured in seconds. Valid inputs are numbers (seconds) or strings parsable by humantime (e.g. \"15days 2min 2s\").",
            "default": null,
            "type": [
              "string",
              "null"
            ]
          },
          "log_visibility": {
            "title": "Log Visibility",
            "description": "Specifies who is allowed to read the canister's logs.\n\nCan be \"public\", \"controllers\" or \"allowed_viewers\" with a list of principals.",
            "default": null,
            "anyOf": [
              {
                "$ref": "#/definitions/CanisterLogVisibility"
              },
              {
                "type": "null"
              }
            ]
          },
          "memory_allocation": {
            "title": "Memory Allocation",
            "description": "Maximum memory (in bytes) this canister is allowed to occupy. Can be specified as an integer, or as an SI unit string (e.g. \"4KB\", \"2 MiB\")",
            "default": null,
            "anyOf": [
              {
                "$ref": "#/definitions/Byte"
              },
              {
                "type": "null"
              }
            ]
          },
          "reserved_cycles_limit": {
            "title": "Reserved Cycles Limit",
            "description": "Specifies the upper limit of the canister's reserved cycles balance.\n\nReserved cycles are cycles that the system sets aside for future use by the canister. If a subnet's storage exceeds 450 GiB, then every time a canister allocates new storage bytes, the system sets aside some amount of cycles from the main balance of the canister. These reserved cycles will be used to cover future payments for the newly allocated bytes. The reserved cycles are not transferable and the amount of reserved cycles depends on how full the subnet is.\n\nA setting of 0 means that the canister will trap if it tries to allocate new storage while the subnet's memory usage exceeds 450 GiB.",
            "default": null,
            "type": [
              "integer",
              "null"
            ],
            "format": "uint128",
            "minimum": 0
          },
          "wasm_memory_limit": {
            "title": "Wasm Memory Limit",
            "description": "Specifies a soft limit (in bytes) on the Wasm memory usage of the canister.\n\nUpdate calls, timers, heartbeats, installs, and post-upgrades fail if the Wasm memory usage exceeds this limit. The main purpose of this setting is to protect against the case when the canister reaches the hard 4GiB limit.\n\nMust be a number of bytes between 0 and 2^48 (i.e. 256 TiB), inclusive. Can be specified as an integer, or as an SI unit string (e.g. \"4KB\", \"2 MiB\")",
            "default": null,
            "anyOf": [
              {
                "$ref": "#/definitions/Byte"
              },
              {
                "type": "null"
              }
            ]
          },
          "wasm_memory_threshold": {
            "title": "Wasm Memory Threshold",
            "description": "Specifies a threshold (in bytes) on the Wasm memory usage of the canister, as a distance from `wasm_memory_limit`.\n\nWhen the remaining memory before the limit drops below this threshold, its `on_low_wasm_memory` hook will be invoked. This enables it to self-optimize, or raise an alert, or otherwise attempt to prevent itself from reaching `wasm_memory_limit`.\n\nMust be a number of bytes between 0 and 2^48 (i.e. 256 TiB), inclusive. Can be specified as an integer, or as an SI unit string (e.g. \"4KB\", \"2 MiB\")",
            "default": null,
            "anyOf": [
              {
                "$ref": "#/definitions/Byte"
              },
              {
                "type": "null"
              }
            ]
          },
          "initial_cycles": {
            "title": "Initial Cycles",
            "description": "Initial amount of cycles that should be transferred to the canister. If this is null then the amount it is up to the installer of the application.",
            "default": null,
            "type": [
              "integer",
              "null"
            ],
            "format": "uint128",
            "minimum": 0
          }
        }
      },
      "PossiblyStr_for_uint64": {
        "type": "integer",
        "format": "uint64",
        "minimum": 0
      },
      "CanisterArg": {
        "title": "Canister Arg",
        "description": "An argument for the canister.",
        "type": "object",
        "properties": {
          "arg": {
            "type": "string"
          },
          "format": {
            "type": "string",
            "enum": ["candid", "json"],
            "default": "candid"
          }
        }
      }
    }
  }
```
