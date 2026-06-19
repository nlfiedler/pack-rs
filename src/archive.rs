//
// Copyright (c) 2024 Nathan Fiedler
//
use crate::util::{decode_link, sanitize_path, symlink_target_within_root, verify_within_root, write_link};
use crate::{is_pack_file, Error, Kind};
use rusqlite::{Connection, DatabaseName};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

///
/// A single entry (file, directory, or symbolic link) in an archive, identified
/// by its full path within the archive.
///
#[derive(Clone, Debug)]
pub struct Entry {
    path: PathBuf,
    kind: Kind,
}

impl Entry {
    /// The full path of this entry within the archive.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The kind of this entry: file, directory, or symbolic link.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Return `true` if this entry is a regular file.
    pub fn is_file(&self) -> bool {
        self.kind == Kind::File
    }

    /// Return `true` if this entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Directory
    }

    /// Return `true` if this entry is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.kind == Kind::Symlink
    }
}

// Result from the IndexedFiles temporary table joined with itemcontent table.
#[derive(Debug)]
struct IndexedFile {
    content: i64,
    contentpos: u64,
    itempos: u64,
    size: u64,
    kind: i64,
    path: String,
}

///
/// Reads and extracts the contents of an archive.
///
/// ```no_run
/// let archive = pack_rs::Archive::open("archive.db3")?;
/// for entry in archive.entries()? {
///     println!("{}", entry.path().display());
/// }
/// archive.unpack("./dest")?;
/// # Ok::<(), pack_rs::Error>(())
/// ```
///
pub struct Archive {
    conn: Connection,
}

impl Archive {
    ///
    /// Open the pack file at the given location for reading. Returns
    /// [`Error::NotPackFile`] if the path is not a recognized archive.
    ///
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        if !is_pack_file(path.as_ref())? {
            return Err(Error::NotPackFile);
        }
        let conn = Connection::open(path.as_ref())?;
        Ok(Self { conn })
    }

    ///
    /// Return all entries in the archive, each with its full path within the
    /// archive, in breadth-first order.
    ///
    pub fn entries(&self) -> Result<Vec<Entry>, Error> {
        //
        // Query from Pack in UPackDraft0Shared.pas that queries all items in
        // ascending order to make it easy to build the results. The path of a
        // directory ends with a slash, which the recursion uses to build the
        // paths of its children; we strip it from the returned entry.
        //
        let query = "WITH RECURSIVE FIT AS (
    SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
    UNION ALL
    SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
        FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
)
SELECT kind, Path FROM FIT;";
        let mut stmt = self.conn.prepare(query)?;
        let rows = stmt.query_map([], |row| {
            let kind: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            Ok((kind, path))
        })?;
        let mut entries: Vec<Entry> = Vec::new();
        for row in rows {
            let (kind, path) = row?;
            entries.push(Entry {
                path: PathBuf::from(path.trim_end_matches('/')),
                kind: Kind::from_i64(kind)?,
            });
        }
        Ok(entries)
    }

    ///
    /// Extract all of the entries in the archive into the destination
    /// directory, creating it if necessary. Returns the number of files
    /// extracted.
    ///
    /// Entries whose paths or symbolic link targets would escape the
    /// destination directory are refused with [`Error::UnsafePath`].
    ///
    pub fn unpack<P: AsRef<Path>>(&self, dest: P) -> Result<u64, Error> {
        // the destination root against which every extracted path is checked to
        // prevent writing outside the destination (e.g. via a symlink)
        fs::create_dir_all(dest.as_ref())?;
        let root = dest.as_ref().canonicalize()?;
        // ensure all of the directories are created, even empty ones
        self.ensure_all_directories(&root)?;
        // create a temporary table for holding the items and their full paths;
        // start by dropping the table in case it was left behind from a
        // previous operation
        self.drop_temp_paths_table()?;
        self.create_temp_paths_table()?;

        // join the item paths with the itemcontent rows and sort by the content
        // blob order, making it easier to efficiently process the content blobs
        let mut stmt = self.conn.prepare(
            "SELECT content, contentpos, itempos, Size, kind, Path FROM IndexedFiles
            LEFT JOIN itemcontent ON IndexedFiles.II = ItemContent.Item
            ORDER BY content, contentpos",
        )?;
        let item_iter = stmt.query_map([], |row| {
            Ok(IndexedFile {
                content: row.get(0)?,
                contentpos: row.get(1)?,
                itempos: row.get(2)?,
                size: row.get(3)?,
                kind: row.get(4)?,
                path: row.get(5)?,
            })
        })?;

        // process the item blobs from the resulting itemcontent query
        let mut content_id: i64 = -1;
        let mut files: Vec<IndexedFile> = vec![];
        let mut file_count: u64 = 0;
        for row_result in item_iter {
            let indexed_file = row_result?;
            if indexed_file.content != content_id {
                // reached the end of the entries for this content
                if !files.is_empty() {
                    file_count += self.process_content(&root, files)?;
                }
                content_id = indexed_file.content;
                files = vec![indexed_file];
            } else {
                // another piece of the same content, add to the list
                files.push(indexed_file);
            }
        }
        // make sure any remaining content is processed
        if !files.is_empty() {
            file_count += self.process_content(&root, files)?;
        }

        // clean up
        self.drop_temp_paths_table()?;
        Ok(file_count)
    }

    // Ensure that all directories in the archive are created, even those that
    // do not contain any files.
    fn ensure_all_directories(&self, root: &Path) -> Result<(), Error> {
        let query = "WITH RECURSIVE FIT AS (
    SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
    UNION ALL
    SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
        FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
)
SELECT Path FROM FIT WHERE Kind = 1;";
        let mut stmt = self.conn.prepare(query)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let fpath = root.join(sanitize_path(path)?);
            fs::create_dir_all(&fpath)?;
            // verify the directory did not resolve outside the root, which can
            // happen if an ancestor on disk is a pre-existing symbolic link
            verify_within_root(root, &fpath)?;
        }
        Ok(())
    }

    // Process a single content blob and all of the files it contains.
    fn process_content(&self, root: &Path, files: Vec<IndexedFile>) -> Result<u64, Error> {
        assert!(!files.is_empty(), "expected files to be non-empty");
        let content_id = files[0].content;

        // fetch the blob and decompress
        let mut blob =
            self.conn
                .blob_open(DatabaseName::Main, "content", "value", content_id, true)?;
        let mut buffer: Vec<u8> = Vec::new();
        zstd::stream::copy_decode(&mut blob, &mut buffer)?;
        drop(blob);

        // process each of the rows of content, which are portions of a file
        let mut file_count: u64 = 0;
        for entry in files.iter() {
            // perform basic sanitization of the file path to prevent abuse (it
            // is theoretically possible that the data could produce a path with
            // a root, prefix, or parent-dir elements). The relative path is kept
            // for the symlink target check; fpath is where the data is written.
            let relpath = sanitize_path(&entry.path)?;
            let fpath = root.join(&relpath);
            match Kind::from_i64(entry.kind)? {
                Kind::File => {
                    // confirm the parent directory resolves within the root
                    // before creating the file, so that a pre-existing symlink
                    // in the destination cannot redirect the write outside it
                    if let Some(parent) = fpath.parent() {
                        verify_within_root(root, parent)?;
                    }
                    // make sure the file exists and is writable
                    let mut output = fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(&fpath)?;
                    // count each file once, on its first chunk
                    if entry.itempos == 0 {
                        file_count += 1;
                    }
                    // empty files have a single zero-size chunk; non-empty files
                    // may span several chunks written in ascending itempos order
                    if entry.size > 0 {
                        // seek to the correct position within the file for this
                        // chunk; writing past the end zero-fills any gap
                        if entry.itempos > 0 {
                            output.seek(SeekFrom::Start(entry.itempos))?;
                        }
                        // use Cursor because that's seemingly easier than getting a slice
                        let mut cursor = std::io::Cursor::new(&buffer);
                        cursor.seek(SeekFrom::Start(entry.contentpos))?;
                        let mut chunk = cursor.take(entry.size);
                        io::copy(&mut chunk, &mut output)?;
                    }
                    // set the length to exactly the end of this chunk, truncating
                    // any leftover bytes from a previously existing file there
                    output.set_len(entry.itempos + entry.size)?;
                }
                Kind::Symlink => {
                    // use Cursor because that's seemingly easier than getting a slice
                    let mut cursor = std::io::Cursor::new(&buffer);
                    cursor.seek(SeekFrom::Start(entry.contentpos))?;
                    let mut chunk = cursor.take(entry.size);
                    let mut raw_bytes: Vec<u8> = vec![];
                    chunk.read_to_end(&mut raw_bytes)?;
                    // reject links whose target would escape the destination
                    // tree, and confirm the link's own directory is within root
                    let target = decode_link(&raw_bytes)?;
                    if !symlink_target_within_root(&relpath, &target) {
                        return Err(Error::UnsafePath(relpath.to_string_lossy().into_owned()));
                    }
                    if let Some(parent) = fpath.parent() {
                        verify_within_root(root, parent)?;
                    }
                    write_link(&raw_bytes, &fpath)?;
                }
                // directories are created separately by ensure_all_directories
                Kind::Directory => {}
            }
        }

        Ok(file_count)
    }

    // Create a table to hold the item identifiers and their full paths and
    // populate it using the values in the item table.
    fn create_temp_paths_table(&self) -> Result<(), Error> {
        self.conn.execute(
            "CREATE TEMPORARY TABLE IndexedFiles (II INTEGER PRIMARY KEY, kind INTEGER, path TEXT)",
            (),
        )?;
        self.conn.execute(
            "INSERT INTO IndexedFiles SELECT II, kind, Path FROM (
                WITH RECURSIVE FIT AS (
                    SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
                    UNION ALL
                    SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
                        FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
                )
                SELECT id AS II, kind, Path FROM FIT WHERE kind <> 1
            )",
            (),
        )?;
        Ok(())
    }

    // Drop the table that holds the item identifiers and their full paths.
    fn drop_temp_paths_table(&self) -> Result<(), Error> {
        self.conn.execute("DROP TABLE IF EXISTS IndexedFiles", ())?;
        Ok(())
    }
}
