//
// Copyright (c) 2024 Nathan Fiedler
//
use clap::{arg, Command};
use pack_rs::{Archive, Builder, Error, Kind};
use std::path::PathBuf;

///
/// Create a pack file at the given location and add all of the named inputs.
///
/// Returns the total number of files added to the archive.
///
fn create_archive(pack: &str, inputs: Vec<&PathBuf>) -> Result<u64, Error> {
    let path_ref = PathBuf::from(pack);
    // default the archive to a `.db3` extension when none was given
    let path = match path_ref.extension() {
        Some(_) => path_ref,
        None => path_ref.with_extension("db3"),
    };
    let mut builder = Builder::create(path)?;
    let mut file_count: u64 = 0;
    for input in inputs {
        file_count += builder.append_path(input)?;
    }
    builder.finish()?;
    Ok(file_count)
}

///
/// List all file entries in the archive (directories are omitted).
///
fn list_contents(pack: &str) -> Result<(), Error> {
    let archive = Archive::open(pack)?;
    for entry in archive.entries()? {
        if entry.kind() != Kind::Directory {
            println!("{}", entry.path().display());
        }
    }
    Ok(())
}

///
/// Extract all of the files from the archive into the current directory.
///
fn extract_contents(pack: &str) -> Result<u64, Error> {
    let archive = Archive::open(pack)?;
    archive.unpack(".")
}

fn cli() -> Command {
    Command::new("pack-rs")
        .about("Archiver/compressor")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("create")
                .about("Creates an archive from a set of files.")
                .short_flag('c')
                .arg(arg!(pack: <PACK> "File path to which the archive will be written."))
                .arg(
                    arg!(<INPUTS> ... "Files to add to archive")
                        .value_parser(clap::value_parser!(PathBuf)),
                )
                .arg_required_else_help(true),
        )
        .subcommand(
            Command::new("list")
                .about("Lists the contents of an archive.")
                .short_flag('l')
                .arg(arg!(pack: <PACK> "File path specifying the archive to read from."))
                .arg_required_else_help(true),
        )
        .subcommand(
            Command::new("extract")
                .about("Extracts one or more files from an archive.")
                .short_flag('x')
                .arg(arg!(pack: <PACK> "File path specifying the archive to read from."))
                .arg_required_else_help(true),
        )
}

fn main() -> Result<(), Error> {
    let matches = cli().get_matches();
    match matches.subcommand() {
        Some(("create", sub_matches)) => {
            let pack = sub_matches
                .get_one::<String>("pack")
                .map(|s| s.as_str())
                .unwrap_or("pack.db3");
            let inputs = sub_matches
                .get_many::<PathBuf>("INPUTS")
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let file_count = create_archive(pack, inputs)?;
            println!("Added {} files to {}", file_count, pack);
        }
        Some(("list", sub_matches)) => {
            let pack = sub_matches
                .get_one::<String>("pack")
                .map(|s| s.as_str())
                .unwrap_or("pack.db3");
            list_contents(pack)?;
        }
        Some(("extract", sub_matches)) => {
            let pack = sub_matches
                .get_one::<String>("pack")
                .map(|s| s.as_str())
                .unwrap_or("pack.db3");
            let file_count = extract_contents(pack)?;
            println!("Extracted {} files from {}", file_count, pack)
        }
        _ => unreachable!(),
    }
    Ok(())
}
