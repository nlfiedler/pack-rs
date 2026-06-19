//
// Copyright (c) 2026 Nathan Fiedler
//
use pack_rs::{Archive, Builder, Error};
use std::fs;
use std::path::Path;

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

// Build an archive from a tree containing an empty file, an empty directory,
// nested directories, a symlink, and a file large enough to span several
// (deliberately small) content bundles, then extract it and assert the result
// matches the original.
#[test]
fn test_roundtrip() -> Result<(), Error> {
    let src = tempfile::tempdir()?;
    let srcdir = src.path().join("tree");
    fs::create_dir(&srcdir)?;
    fs::write(srcdir.join("empty.txt"), b"")?;
    fs::write(srcdir.join("hello.txt"), b"hello world")?;
    fs::create_dir(srcdir.join("emptydir"))?;
    fs::create_dir_all(srcdir.join("a/b"))?;
    fs::write(srcdir.join("a/b/nested.txt"), b"nested content")?;
    // a file larger than the bundle size set below, so it spans bundles
    let big: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    fs::write(srcdir.join("big.bin"), &big)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("hello.txt", srcdir.join("link.txt"))?;

    let archive_path = src.path().join("archive.db3");
    {
        // a small bundle size forces big.bin to span multiple content blobs
        let mut builder = Builder::new()?.bundle_size(1024);
        let count = builder.append_dir_all(&srcdir)?;
        builder.finish(&archive_path)?;
        // four regular files; the symlink is not counted
        assert_eq!(count, 4);
    }

    let out = tempfile::tempdir()?;
    let archive = Archive::open(&archive_path)?;
    let extracted = archive.unpack(out.path())?;
    assert_eq!(extracted, 4);

    // archived paths are rooted at the base directory's name ("tree")
    let base = out.path().join("tree");
    assert_eq!(fs::metadata(base.join("empty.txt"))?.len(), 0);
    assert_eq!(read(&base.join("hello.txt")), b"hello world");
    assert!(base.join("emptydir").is_dir());
    assert_eq!(read(&base.join("a/b/nested.txt")), b"nested content");
    assert_eq!(read(&base.join("big.bin")), big);
    #[cfg(unix)]
    {
        let target = fs::read_link(base.join("link.txt"))?;
        assert_eq!(target, Path::new("hello.txt"));
    }
    Ok(())
}

// An archive containing a symlink whose target escapes the destination must be
// refused during extraction.
#[cfg(unix)]
#[test]
fn test_unpack_rejects_escaping_symlink() -> Result<(), Error> {
    let src = tempfile::tempdir()?;
    let srcdir = src.path().join("tree");
    fs::create_dir(&srcdir)?;
    std::os::unix::fs::symlink("/etc/passwd", srcdir.join("escape"))?;

    let archive_path = src.path().join("evil.db3");
    {
        let mut builder = Builder::new()?;
        builder.append_dir_all(&srcdir)?;
        builder.finish(&archive_path)?;
    }

    let out = tempfile::tempdir()?;
    let archive = Archive::open(&archive_path)?;
    let result = archive.unpack(out.path());
    assert!(
        matches!(result, Err(Error::UnsafePath(_))),
        "expected UnsafePath, got {:?}",
        result
    );
    Ok(())
}
