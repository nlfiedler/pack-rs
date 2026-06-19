//
// Copyright (c) 2026 Nathan Fiedler
//
use crate::util::{get_file_name, read_link};
use crate::{Error, Kind};
use rusqlite::{params, Connection};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Default target size, in bytes, of a content bundle (16 MiB).
const DEFAULT_BUNDLE_SIZE: u64 = 16 * 1024 * 1024;
/// Default Zstandard compression level (0 selects the zstd default).
const DEFAULT_COMPRESSION_LEVEL: i32 = 0;

//
// Create the database tables if they do not exist.
//
fn create_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS item (
            id INTEGER PRIMARY KEY,
            parent INTEGER,
            kind INTEGER,
            name TEXT NOT NULL
        )",
        (),
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS content (
            id INTEGER PRIMARY KEY,
            value BLOB
        )",
        (),
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS itemcontent (
            id INTEGER PRIMARY KEY,
            item INTEGER,
            itempos INTEGER,
            content INTEGER,
            contentpos INTEGER,
            size INTEGER
        )",
        (),
    )?;
    Ok(())
}

//
// Represents the content of a file (item) and its position within a content
// bundle when building an archive. It is possible that a portion of the file is
// being added and thus the itempos might be non-zero; similarly the size may be
// less than the actual file length.
//
struct IncomingContent {
    // path of the file being packed
    path: PathBuf,
    // kind of item: file or symlink
    kind: Kind,
    // the rowid in the item table
    item: i64,
    // offset within the file from which to start, usually zero
    itempos: u64,
    // offset within the content bundle where the data will go
    contentpos: u64,
    // size of the item content
    size: u64,
}

///
/// Builds an archive by appending files and directories.
///
/// The archive is written directly to the destination file given to
/// [`Builder::create`]. The entire build runs inside a single SQLite
/// transaction that is committed by [`Builder::finish`], so the many small row
/// inserts incur a single commit rather than one synchronous commit per
/// statement. If the `Builder` is dropped without calling `finish`, the
/// transaction is rolled back and no archive is produced.
///
/// ```no_run
/// let mut builder = pack_rs::Builder::create("archive.db3")?;
/// builder.append_dir_all("src")?;
/// builder.finish()?;
/// # Ok::<(), pack_rs::Error>(())
/// ```
///
pub struct Builder {
    // database connection to the destination file
    conn: Connection,
    // target size of a content bundle, in bytes
    bundle_size: u64,
    // Zstandard compression level
    level: i32,
    // byte offset within a bundle to which new content is added
    current_pos: u64,
    // item content that will reside in the bundle under construction
    contents: Vec<IncomingContent>,
    // workspace for compressing the content bundles
    buffer: Option<Vec<u8>>,
}

impl Builder {
    ///
    /// Create a new `Builder` writing to the archive file at `dest`, replacing
    /// any existing file at that path.
    ///
    /// The build proceeds inside a single transaction; call [`Builder::finish`]
    /// to commit it.
    ///
    pub fn create<P: AsRef<Path>>(dest: P) -> Result<Self, Error> {
        // start from a clean database file so this is a create, not an append
        let _ = fs::remove_file(dest.as_ref());
        let conn = Connection::open(dest.as_ref())?;
        // can set the page_size when creating the database, but not after
        // conn.pragma_update(None, "page_size", 512)?;
        create_tables(&conn)?;
        // Build the whole archive inside one transaction. With the default
        // (synchronous=FULL) durability this still commits/fsyncs only once at
        // finish, instead of once per statement as autocommit would.
        conn.execute_batch("BEGIN")?;
        Ok(Self {
            conn,
            bundle_size: DEFAULT_BUNDLE_SIZE,
            level: DEFAULT_COMPRESSION_LEVEL,
            current_pos: 0,
            contents: vec![],
            buffer: None,
        })
    }

    ///
    /// Set the target size, in bytes, of the content bundles. Larger bundles
    /// can compress better while using more memory. Defaults to 16 MiB.
    ///
    pub fn bundle_size(mut self, bytes: u64) -> Self {
        self.bundle_size = bytes;
        self
    }

    ///
    /// Set the Zstandard compression level. A level of `0` selects the zstd
    /// default. Defaults to `0`.
    ///
    pub fn compression_level(mut self, level: i32) -> Self {
        self.level = level;
        self
    }

    ///
    /// Append a single file to the archive at the root of the archive.
    ///
    /// **Note:** Remember to call [`Builder::finish`] when done adding content.
    ///
    pub fn append_file<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.add_file(path.as_ref(), 0)?;
        Ok(())
    }

    ///
    /// Append a path to the archive, recursing into it if it is a directory.
    ///
    /// Returns the number of files added. Symbolic links are stored as links
    /// (not followed) and do not contribute to the count.
    ///
    /// **Note:** Remember to call [`Builder::finish`] when done adding content.
    ///
    pub fn append_path<P: AsRef<Path>>(&mut self, path: P) -> Result<u64, Error> {
        // symlink_metadata so that a top-level symlink is stored, not followed
        let metadata = fs::symlink_metadata(path.as_ref())?;
        if metadata.is_dir() {
            self.append_dir_all(path)
        } else if metadata.is_symlink() {
            self.add_symlink(path.as_ref(), 0)?;
            Ok(0)
        } else {
            self.add_file(path.as_ref(), 0)?;
            Ok(1)
        }
    }

    ///
    /// Visit all of the files and directories within the specified path, adding
    /// them to the archive. Returns the number of files added.
    ///
    /// **Note:** Remember to call [`Builder::finish`] when done adding content.
    ///
    pub fn append_dir_all<P: AsRef<Path>>(&mut self, basepath: P) -> Result<u64, Error> {
        let mut file_count: u64 = 0;
        // used as a stack (push/pop), so directories are visited depth-first
        let mut subdirs: Vec<(i64, PathBuf)> = Vec::new();
        subdirs.push((0, basepath.as_ref().to_path_buf()));
        while let Some((mut parent_id, currdir)) = subdirs.pop() {
            parent_id = self.add_directory(&currdir, parent_id)?;
            let readdir = fs::read_dir(currdir)?;
            for entry_result in readdir {
                let entry = entry_result?;
                let path = entry.path();
                // DirEntry.metadata() does not follow symlinks and that is good
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    subdirs.push((parent_id, path));
                } else if metadata.is_file() {
                    self.add_file(&path, parent_id)?;
                    file_count += 1;
                } else if metadata.is_symlink() {
                    self.add_symlink(&path, parent_id)?;
                }
            }
        }
        Ok(file_count)
    }

    ///
    /// Finish building the archive, committing the transaction so the archive
    /// file is complete. Consumes the builder.
    ///
    pub fn finish(mut self) -> Result<(), Error> {
        if !self.contents.is_empty() {
            self.process_contents()?;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    //
    // Process the current bundle of item content, clearing the collection and
    // resetting the current content position.
    //
    fn process_contents(&mut self) -> Result<(), Error> {
        self.insert_content()?;
        self.contents = vec![];
        self.current_pos = 0;
        Ok(())
    }

    //
    // Add a row to the `item` table that corresponds to this directory.
    //
    fn add_directory<P: AsRef<Path>>(&self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, Kind::Directory.as_i64(), &name),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    //
    // Adds a single file to the archive, returning the item identifier.
    //
    // Depending on the size of the file and the content bundle so far, this may
    // result in writing one or more rows to the content and itemcontent tables.
    //
    fn add_file<P: AsRef<Path>>(&mut self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, Kind::File.as_i64(), &name),
        )?;
        let item_id = self.conn.last_insert_rowid();
        // propagate metadata errors rather than silently archiving the file as
        // empty, which would be silent data loss
        let file_len = fs::metadata(path.as_ref())?.len();
        // empty files will result in an itemcontent row whose size is zero,
        // allowing for the extraction process to know to create an empty file
        // (otherwise it is difficult to tell from the available data)
        let mut itempos: u64 = 0;
        let mut size: u64 = file_len;
        loop {
            if self.current_pos + size > self.bundle_size {
                let remainder = self.bundle_size - self.current_pos;
                // only add a partial chunk when there is room left in the
                // bundle; when the bundle is already full (remainder == 0) just
                // flush it and retry, avoiding a spurious zero-size chunk
                if remainder > 0 {
                    let content = IncomingContent {
                        path: path.as_ref().to_path_buf(),
                        kind: Kind::File,
                        item: item_id,
                        itempos,
                        contentpos: self.current_pos,
                        size: remainder,
                    };
                    self.contents.push(content);
                    size -= remainder;
                    itempos += remainder;
                }
                // insert the content and itemcontent rows and start a new
                // bundle, then continue with the current file
                self.process_contents()?;
            } else {
                // the remainder of the file fits within this content bundle
                let content = IncomingContent {
                    path: path.as_ref().to_path_buf(),
                    kind: Kind::File,
                    item: item_id,
                    itempos,
                    contentpos: self.current_pos,
                    size,
                };
                self.contents.push(content);
                self.current_pos += size;
                break;
            }
        }
        Ok(item_id)
    }

    //
    // Adds a symbolic link to the archive, returning the item identifier.
    //
    fn add_symlink<P: AsRef<Path>>(&mut self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, Kind::Symlink.as_i64(), &name),
        )?;
        let item_id = self.conn.last_insert_rowid();
        // derive the size from the actual link bytes (rather than
        // symlink_metadata) so it matches exactly what insert_content writes,
        // and propagate errors instead of silently storing an empty link
        let link_len = read_link(path.as_ref())?.len() as u64;
        // assume that the link value is relatively small and simply add it into
        // the current content bundle in whole
        let content = IncomingContent {
            path: path.as_ref().to_path_buf(),
            kind: Kind::Symlink,
            item: item_id,
            itempos: 0,
            contentpos: self.current_pos,
            size: link_len,
        };
        self.contents.push(content);
        self.current_pos += link_len;
        Ok(item_id)
    }

    //
    // Creates a content bundle based on the data collected so far, then
    // compresses it, writing the blob to a new row in the `content` table. Then
    // creates the necessary rows in the `itemcontent` table to map the file
    // data to the content bundle.
    //
    fn insert_content(&mut self) -> Result<(), Error> {
        // Allocate a buffer for the compressed data, reusing it each time. For
        // small data sets this makes no observable difference, but for any
        // large data set (e.g. Linux kernel), it makes a huge difference.
        let content: Vec<u8> = if let Some(mut buf) = self.buffer.take() {
            buf.clear();
            buf
        } else {
            Vec::with_capacity(self.bundle_size as usize)
        };
        let mut encoder = zstd::stream::write::Encoder::new(content, self.level)?;

        // iterate through the file contents to build the compressed bundle
        for item in self.contents.iter() {
            if item.kind == Kind::File {
                let mut input = fs::File::open(&item.path)?;
                input.seek(SeekFrom::Start(item.itempos))?;
                let mut chunk = input.take(item.size);
                // the contentpos/size offsets recorded for every item in this
                // bundle were computed during planning; if the file changed
                // size since then, the copied length will not match and every
                // following item in the bundle would be misaligned, so treat a
                // short/long read as a hard error rather than silent corruption
                let copied = io::copy(&mut chunk, &mut encoder)?;
                if copied != item.size {
                    return Err(Error::ContentSizeMismatch(
                        item.path.to_string_lossy().into_owned(),
                    ));
                }
            } else if item.kind == Kind::Symlink {
                let value = read_link(&item.path)?;
                if value.len() as u64 != item.size {
                    return Err(Error::ContentSizeMismatch(
                        item.path.to_string_lossy().into_owned(),
                    ));
                }
                encoder.write_all(&value)?;
            }
        }
        let content = encoder.finish()?;

        // Bind the compressed bundle directly as a blob parameter. The data is
        // already fully in memory, so the zeroblob + incremental-write idiom
        // (which exists for streaming) would only write the bytes twice.
        self.conn.execute(
            "INSERT INTO content (value) VALUES (?1)",
            params![content],
        )?;
        let content_id = self.conn.last_insert_rowid();
        // reclaim the buffer for reuse on the next bundle
        self.buffer = Some(content);

        // iterate through the item contents and insert new itemcontent rows
        for item in self.contents.iter() {
            // create the mapping for this bit of content
            self.conn.execute(
                "INSERT INTO itemcontent (
                    item, itempos, content, contentpos, size
                ) VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    &item.item,
                    &item.itempos,
                    &content_id,
                    &item.contentpos,
                    &item.size,
                ),
            )?;
        }

        Ok(())
    }
}
