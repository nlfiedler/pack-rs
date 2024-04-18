//
// Copyright (c) 2024 Nathan Fiedler
//
use rusqlite::Connection;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

///
/// This type represents all possible errors that can occur within this crate.
///
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Error occurred during an I/O related operation.
    #[error("I/O error: {0}")]
    IOError(#[from] std::io::Error),
    /// Error occurred during an SQL related operation.
    #[error("SQL error: {0}")]
    SQLError(#[from] rusqlite::Error),
    /// When writing file content to a blob, the result was incomplete.
    #[error("could not write entire file part to blob")]
    IncompleteBlobWrite,
    /// The named pack file was not one of ours.
    #[error("pack file format not recognized")]
    NotPackFile,
    /// The symbolic link bytes were not decipherable.
    #[error("symbolic link encoding was not recognized")]
    LinkTextEncoding,
    /// Something happened when operating on the database.
    #[error("error resulting from database operation")]
    Database,
    /// Thread pool is shutting down
    #[error("thread pool is shutting down")]
    ThreadPoolShutdown,
}

// Expected SQLite database header: "SQLite format 3\0"
static SQL_HEADER: &'static [u8] = &[
    0x53, 0x51, 0x4c, 0x69, 0x74, 0x65, 0x20, 0x66, 0x6f, 0x72, 0x6d, 0x61, 0x74, 0x20, 0x33, 0x00,
];

///
/// Return `true` if the path refers to a pack file, false otherwise.
///
pub fn is_pack_file<P: AsRef<Path>>(path: P) -> Result<bool, Error> {
    let metadata = fs::metadata(path.as_ref())?;
    if metadata.is_file() && metadata.len() > 16 {
        let mut file = fs::File::open(path.as_ref())?;
        let mut buffer = [0; 16];
        file.read_exact(&mut buffer)?;
        if buffer == SQL_HEADER {
            // open and check for non-zero amount of data
            let conn = Connection::open(path.as_ref())?;
            match conn.prepare("SELECT * FROM item") {
                Ok(mut stmt) => {
                    let result = stmt.exists([])?;
                    return Ok(result);
                }
                Err(_) => {
                    return Ok(false);
                }
            };
        }
    }
    Ok(false)
}

///
/// Return a sanitized version of the path, with any non-normal components
/// removed. Roots and prefixes are especially problematic for extracting an
/// archive, so those are always removed. Note also that path components which
/// refer to the parent directory will be stripped ("foo/../bar" will become
/// "foo/bar").
///
pub fn sanitize_path<P: AsRef<Path>>(dirty: P) -> Result<PathBuf, Error> {
    let components = dirty.as_ref().components();
    let allowed = components.filter(|c| matches!(c, Component::Normal(_)));
    let mut path = PathBuf::new();
    for component in allowed {
        path = path.join(component);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pack_file() -> Result<(), Error> {
        assert!(!is_pack_file("test/fixtures/empty-file")?);
        assert!(!is_pack_file("test/fixtures/notpack.db3")?);
        assert!(is_pack_file("test/fixtures/pack.db3")?);
        Ok(())
    }

    #[test]
    fn test_sanitize_path() -> Result<(), Error> {
        // need to use real paths for the canonicalize() call
        #[cfg(target_family = "windows")]
        {
            let result = sanitize_path(Path::new("C:\\Windows"))?;
            assert_eq!(result, PathBuf::from("Windows"));
        }
        #[cfg(target_family = "unix")]
        {
            let result = sanitize_path(Path::new("/etc"))?;
            assert_eq!(result, PathBuf::from("etc"));
        }
        let result = sanitize_path(Path::new("src/lib.rs"))?;
        assert_eq!(result, PathBuf::from("src/lib.rs"));

        let result = sanitize_path(Path::new("/usr/../src/./lib.rs"))?;
        assert_eq!(result, PathBuf::from("usr/src/lib.rs"));
        Ok(())
    }
}
