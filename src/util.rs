//
// Copyright (c) 2026 Nathan Fiedler
//
use crate::Error;
use rusqlite::Connection;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

// Expected SQLite database header: "SQLite format 3\0"
static SQL_HEADER: &[u8] = &[
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

///
/// Return `true` if the symbolic link `target` stays within the destination
/// directory when resolved relative to the link's own location, `false`
/// otherwise.
///
/// `link_path` is the link's (already sanitized) path relative to the
/// destination root. Resolution is purely lexical so that it is not fooled by
/// other symbolic links on the filesystem. Absolute targets, and relative
/// targets that climb above the root via `..`, are rejected.
///
pub fn symlink_target_within_root<P: AsRef<Path>>(link_path: P, target: P) -> bool {
    if target.as_ref().is_absolute() {
        return false;
    }
    // depth of the directory containing the link, relative to the root
    let mut depth: i64 = link_path
        .as_ref()
        .parent()
        .map(|p| {
            p.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count() as i64
        })
        .unwrap_or(0);
    for component in target.as_ref().components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            // a root or prefix component means the target escapes the tree
            _ => return false,
        }
    }
    true
}

///
/// Return the last part of the path, converting to a String.
///
pub(crate) fn get_file_name<P: AsRef<Path>>(path: P) -> String {
    // ignore any paths that end in '..'
    if let Some(p) = path.as_ref().file_name() {
        // ignore any paths that failed UTF-8 translation
        if let Some(pp) = p.to_str() {
            return pp.to_owned();
        }
    }
    // normal conversion failed, return whatever garbage is there
    path.as_ref().to_string_lossy().into_owned()
}

///
/// Read the symbolic link value and convert to raw bytes.
///
pub(crate) fn read_link(path: &Path) -> Result<Vec<u8>, Error> {
    // convert whatever value returned by the OS into raw bytes without string conversion
    use os_str_bytes::OsStringBytes;
    let value = fs::read_link(path)?;
    Ok(value.into_os_string().into_raw_vec())
}

///
/// Decode raw symbolic link bytes into a path for the current platform.
///
pub(crate) fn decode_link(contents: &[u8]) -> Result<PathBuf, Error> {
    use os_str_bytes::OsStringBytes;
    // this returns None if the bytes are not valid for this platform
    let target =
        std::ffi::OsString::from_io_vec(contents.to_owned()).ok_or(Error::LinkTextEncoding)?;
    Ok(PathBuf::from(target))
}

///
/// Create a symbolic link using the given raw bytes.
///
pub(crate) fn write_link(contents: &[u8], filepath: &Path) -> Result<(), Error> {
    let target = decode_link(contents)?;
    // cfg! macro will not work in this OS-specific import case
    {
        #[cfg(target_family = "unix")]
        use std::os::unix::fs;
        #[cfg(target_family = "windows")]
        use std::os::windows::fs;
        #[cfg(target_family = "unix")]
        fs::symlink(target, filepath)?;
        #[cfg(target_family = "windows")]
        fs::symlink_file(target, filepath)?;
    }
    Ok(())
}

///
/// Verify that `path`, after resolving any symbolic links on disk, stays within
/// `root`. Returns `Error::UnsafePath` if it escapes. The path must already
/// exist on disk (it is canonicalized).
///
pub(crate) fn verify_within_root(root: &Path, path: &Path) -> Result<(), Error> {
    let canon = path.canonicalize()?;
    if !canon.starts_with(root) {
        return Err(Error::UnsafePath(path.to_string_lossy().into_owned()));
    }
    Ok(())
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

    #[test]
    fn test_symlink_target_within_root() {
        // simple relative targets that stay inside the tree
        assert!(symlink_target_within_root("link", "target"));
        assert!(symlink_target_within_root("dir/link", "target"));
        assert!(symlink_target_within_root("dir/link", "../sibling"));
        assert!(symlink_target_within_root("a/b/link", "../../top"));
        assert!(symlink_target_within_root("dir/link", "./nested/file"));

        // targets that climb above the root are rejected
        assert!(!symlink_target_within_root("link", ".."));
        assert!(!symlink_target_within_root("dir/link", "../.."));
        assert!(!symlink_target_within_root("a/b/link", "../../../escape"));

        // absolute targets are always rejected
        assert!(!symlink_target_within_root("link", "/etc/passwd"));
    }
}
