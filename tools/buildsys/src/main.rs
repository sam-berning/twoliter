/*!
This tool carries out a package or variant build using Docker.

It is meant to be called by a Cargo build script. To keep those scripts simple,
all of the configuration is taken from the environment, with the build type
specified as a command line argument.

The implementation is closely tied to the top-level Dockerfile.

*/
mod args;
mod builder;
mod cache;
mod gomod;
mod project;
mod spec;

use crate::args::{BuildPackageArgs, BuildVariantArgs, Buildsys, Command};
use crate::builder::DockerBuild;
use buildsys::manifest::{BundleModule, ManifestInfo, SupportedArch};
use cache::LookasideCache;
use clap::Parser;
use gomod::GoMod;
use merge_toml::merge_values;
use project::ProjectInfo;
use snafu::{ensure, ResultExt};
use spec::SpecInfo;
use std::path::{Path, PathBuf};
use std::{fs, process};
use toml::{map::Map, Value};
use walkdir::WalkDir;

mod error {
    use buildsys::manifest::SupportedArch;
    use snafu::Snafu;
    use std::path::PathBuf;

    #[derive(Debug, Snafu)]
    #[snafu(visibility(pub(super)))]
    pub(super) enum Error {
        ManifestParse {
            source: buildsys::manifest::Error,
        },

        SpecParse {
            source: super::spec::error::Error,
        },

        ExternalFileFetch {
            source: super::cache::error::Error,
        },

        GoMod {
            source: super::gomod::error::Error,
        },

        ProjectCrawl {
            source: super::project::error::Error,
        },

        BuildAttempt {
            source: super::builder::error::Error,
        },

        #[snafu(display("Unable to instantiate the builder: {source}"))]
        BuilderInstantiation {
            source: crate::builder::error::Error,
        },

        #[snafu(display("Missing environment variable '{}'", var))]
        Environment {
            var: String,
            source: std::env::VarError,
        },

        #[snafu(display("Unknown architecture: '{}'", arch))]
        UnknownArch {
            arch: String,
            source: serde_plain::Error,
        },

        #[snafu(display(
            "Unsupported architecture {}, this variant supports {}",
            arch,
            supported_arches.join(", ")
        ))]
        UnsupportedArch {
            arch: SupportedArch,
            supported_arches: Vec<String>,
        },

        #[snafu(display("Failed to {} {}: {}", op, path.display(), source))]
        File {
            op: String,
            path: PathBuf,
            source: std::io::Error,
        },

        #[snafu(display("Failed to list files in {}: {}", dir.display(), source))]
        ListFiles {
            dir: PathBuf,
            source: walkdir::Error,
        },

        #[snafu(display("{} is not valid TOML: {}", path.display(), source))]
        TomlDeserialize {
            path: PathBuf,
            source: toml::de::Error,
        },

        #[snafu(display("Failed to merge TOML: {}", source))]
        TomlMerge {
            source: merge_toml::Error,
        },

        #[snafu(display("Failed to serialize default settings: {}", source))]
        TomlSerialize {
            source: toml::ser::Error,
        },
    }
}

type Result<T> = std::result::Result<T, error::Error>;

// Returning a Result from main makes it print a Debug representation of the error, but with Snafu
// we have nice Display representations of the error, so we wrap "main" (run) and print any error.
// https://github.com/shepmaster/snafu/issues/110
fn main() {
    let args = Buildsys::parse();
    if let Err(e) = run(args) {
        eprintln!("{}", e);
        process::exit(1);
    }
}

fn run(args: Buildsys) -> Result<()> {
    args::rerun_for_envs(args.command.build_type());
    match args.command {
        Command::BuildPackage(args) => build_package(*args),
        Command::BuildVariant(args) => build_variant(*args),
    }
}

fn build_package(args: BuildPackageArgs) -> Result<()> {
    let manifest_file = "Cargo.toml";
    println!("cargo:rerun-if-changed={}", manifest_file);

    let variant_manifest_path = args
        .common
        .root_dir
        .join("variants")
        .join(&args.variant)
        .join(manifest_file);
    let variant_manifest =
        ManifestInfo::new(variant_manifest_path).context(error::ManifestParseSnafu)?;
    supported_arch(&variant_manifest, args.common.arch)?;
    let mut image_features = variant_manifest.image_features();

    let manifest = ManifestInfo::new(args.common.cargo_manifest_dir.join(manifest_file))
        .context(error::ManifestParseSnafu)?;
    let package_features = manifest.package_features();

    // For any package feature specified in the package manifest, track the corresponding
    // environment variable for changes to the ambient set of image features for the current
    // variant.
    if let Some(package_features) = &package_features {
        for package_feature in package_features {
            println!(
                "cargo:rerun-if-env-changed=BUILDSYS_VARIANT_IMAGE_FEATURE_{}",
                package_feature
            );
        }
    }

    // Keep only the image features that the package has indicated that it tracks, if any.
    if let Some(image_features) = &mut image_features {
        match package_features {
            Some(package_features) => image_features.retain(|k| package_features.contains(k)),
            None => image_features.clear(),
        }
    }

    // If manifest has package.metadata.build-package.variant-sensitive set, then track the
    // appropriate environment variable for changes.
    if let Some(sensitivity) = manifest.variant_sensitive() {
        use buildsys::manifest::{SensitivityType::*, VariantSensitivity::*};
        fn emit_variant_env(suffix: Option<&str>) {
            if let Some(suffix) = suffix {
                println!(
                    "cargo:rerun-if-env-changed=BUILDSYS_VARIANT_{}",
                    suffix.to_uppercase()
                );
            } else {
                println!("cargo:rerun-if-env-changed=BUILDSYS_VARIANT");
            }
        }
        match sensitivity {
            Any(false) => (),
            Any(true) => emit_variant_env(None),
            Specific(Platform) => emit_variant_env(Some("platform")),
            Specific(Runtime) => emit_variant_env(Some("runtime")),
            Specific(Family) => emit_variant_env(Some("family")),
            Specific(Flavor) => emit_variant_env(Some("flavor")),
        }
    }

    if let Some(files) = manifest.external_files() {
        let lookaside_cache = LookasideCache::new(
            &args.common.version_full,
            args.lookaside_cache.clone(),
            args.upstream_source_fallback == "true",
        );
        lookaside_cache
            .fetch(files)
            .context(error::ExternalFileFetchSnafu)?;
        for f in files {
            if f.bundle_modules.is_none() {
                continue;
            }

            for b in f.bundle_modules.as_ref().unwrap() {
                match b {
                    BundleModule::Go => GoMod::vendor(
                        &args.common.root_dir,
                        &args.common.cargo_manifest_dir,
                        f,
                        &args.common.sdk_image,
                    )
                    .context(error::GoModSnafu)?,
                }
            }
        }
    }

    if let Some(groups) = manifest.source_groups() {
        let dirs = groups
            .iter()
            .map(|d| args.sources_dir.join(d))
            .collect::<Vec<_>>();
        let info = ProjectInfo::crawl(&dirs).context(error::ProjectCrawlSnafu)?;
        for f in info.files {
            println!("cargo:rerun-if-changed={}", f.display());
        }
    }

    // Package developer can override name of package if desired, e.g. to name package with
    // characters invalid in Cargo crate names
    let package = if let Some(name_override) = manifest.package_name() {
        name_override.clone()
    } else {
        args.cargo_package_name.clone()
    };
    let spec = format!("{}.spec", package);
    println!("cargo:rerun-if-changed={}", spec);

    let info = SpecInfo::new(PathBuf::from(&spec)).context(error::SpecParseSnafu)?;

    for f in info.sources {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    for f in info.patches {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    DockerBuild::new_package(args, &manifest, image_features.unwrap_or_default())
        .context(error::BuilderInstantiationSnafu)?
        .build()
        .context(error::BuildAttemptSnafu)?;
    Ok(())
}

fn build_variant(args: BuildVariantArgs) -> Result<()> {
    let manifest_file = "Cargo.toml";
    println!("cargo:rerun-if-changed={}", manifest_file);

    let manifest = ManifestInfo::new(args.common.cargo_manifest_dir.join(manifest_file))
        .context(error::ManifestParseSnafu)?;

    supported_arch(&manifest, args.common.arch)?;

    generate_defaults_toml(&manifest, &args.common.root_dir)?;

    if manifest.included_packages().is_some() {
        DockerBuild::new_variant(args, &manifest)
            .context(error::BuilderInstantiationSnafu)?
            .build()
            .context(error::BuildAttemptSnafu)?;
    } else {
        println!("cargo:warning=No included packages in manifest. Skipping variant build.");
    }
    Ok(())
}

/// Ensure that the current arch is supported by the current variant
fn supported_arch(manifest: &ManifestInfo, arch: SupportedArch) -> Result<()> {
    if let Some(supported_arches) = manifest.supported_arches() {
        ensure!(
            supported_arches.contains(&arch),
            error::UnsupportedArchSnafu {
                arch,
                supported_arches: supported_arches
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<String>>()
            }
        )
    }
    Ok(())
}

/// Merge the variant's default settings files into a single TOML value.  The result is serialized
/// to a file in OUT_DIR for storewolf to read.
fn generate_defaults_toml(manifest: &ManifestInfo, root_dir: &PathBuf) -> Result<()> {
    if let Some(defaults_dir) = manifest.defaults_dir() {
        // Find TOML config files specified by the variant.
        let walker = WalkDir::new(defaults_dir)
            .follow_links(true) // we expect users to link to shared files
            .min_depth(1) // only read files in defaults.d, not doing inheritance yet
            .max_depth(1)
            .sort_by(|a, b| a.file_name().cmp(b.file_name())) // allow ordering by prefix
            .into_iter()
            .filter_entry(|e| e.file_name().to_string_lossy().ends_with(".toml")); // looking for TOML config

        // Merge the files into a single TOML value, in order.
        let mut defaults = Value::Table(Map::new());
        for entry in walker {
            let entry = entry.context(error::ListFilesSnafu { dir: defaults_dir })?;

            // Reflect that we need to rerun if any of the default settings files have changed.
            println!("cargo:rerun-if-changed={}", entry.path().display());

            let data = fs::read_to_string(entry.path()).context(error::FileSnafu {
                op: "read",
                path: entry.path(),
            })?;
            let value = toml::from_str(&data)
                .context(error::TomlDeserializeSnafu { path: entry.path() })?;
            merge_values(&mut defaults, &value).context(error::TomlMergeSnafu)?;
        }

        // Serialize to disk.
        let data = toml::to_string(&defaults).context(error::TomlSerializeSnafu)?;
        // FIXME: need a better spot for this. ./build/static? maybe simply ./build?
        let path = Path::new(root_dir).join("build/tools/defaults.toml");
        fs::write(&path, data).context(error::FileSnafu { op: "write", path })?;
    }
    Ok(())
}
