use clap::Subcommand;

pub(crate) mod create;

/// Build an ICP application bundle (zip file) containing canister
/// wasms and a manifest describing how to install them.
///
/// This command implements a subset of the packaging design in
/// `packaging_design.md`. See the design doc for the full format; this
/// MVP only exposes the fields our demo users actually care about:
///   - init_arg
///   - dependencies
///   - upgrade_arg
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Create(create::CreateArgs),
}
