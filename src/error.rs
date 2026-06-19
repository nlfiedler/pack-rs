//
// Copyright (c) 2026 Nathan Fiedler
//

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
    /// A file shrank or grew between planning and writing the archive.
    #[error("file size changed while building archive: {0}")]
    ContentSizeMismatch(String),
    /// An archive entry would be extracted outside the destination directory.
    #[error("refusing to extract entry outside destination: {0}")]
    UnsafePath(String),
    /// An item in the archive has an unrecognized kind value.
    #[error("unrecognized item kind: {0}")]
    UnknownKind(i64),
}
