//
// Copyright (c) 2024 Nathan Fiedler
//
use crate::Error;

///
/// The type of an item stored in an archive: a regular file, a directory, or a
/// symbolic link.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
}

impl Kind {
    /// Return the integer used to represent this kind in the database.
    pub(crate) fn as_i64(self) -> i64 {
        match self {
            Kind::File => 0,
            Kind::Directory => 1,
            Kind::Symlink => 2,
        }
    }

    /// Convert the database integer representation into a `Kind`.
    pub(crate) fn from_i64(value: i64) -> Result<Self, Error> {
        match value {
            0 => Ok(Kind::File),
            1 => Ok(Kind::Directory),
            2 => Ok(Kind::Symlink),
            other => Err(Error::UnknownKind(other)),
        }
    }
}
