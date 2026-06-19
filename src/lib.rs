//
// Copyright (c) 2024 Nathan Fiedler
//

//! `pack-rs` is an archiver/compressor that stores archives as
//! [SQLite](https://www.sqlite.org) databases, with file data held in large
//! blobs compressed using [Zstandard](http://facebook.github.io/zstd/).
//!
//! The API resembles that of the [`tar`](https://crates.io/crates/tar) crate:
//! build an archive with a [`Builder`], and read or extract one with an
//! [`Archive`].
//!
//! # Creating an archive
//!
//! ```no_run
//! let mut builder = pack_rs::Builder::create("archive.db3")?;
//! builder.append_dir_all("src")?;
//! builder.finish()?;
//! # Ok::<(), pack_rs::Error>(())
//! ```
//!
//! # Reading and extracting an archive
//!
//! ```no_run
//! let archive = pack_rs::Archive::open("archive.db3")?;
//! for entry in archive.entries()? {
//!     println!("{}", entry.path().display());
//! }
//! archive.unpack("./dest")?;
//! # Ok::<(), pack_rs::Error>(())
//! ```

mod archive;
mod builder;
mod error;
mod kind;
mod util;

pub use archive::{Archive, Entry};
pub use builder::Builder;
pub use error::Error;
pub use kind::Kind;
pub use util::{is_pack_file, sanitize_path, symlink_target_within_root};
