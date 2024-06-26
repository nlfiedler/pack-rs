//
// Copyright (c) 2024 Nathan Fiedler
//
use clap::{arg, Command};
use pack_rs::Error;
use rusqlite::{Connection, DatabaseName};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::vec;

const KIND_FILE: i8 = 0;
const KIND_DIRECTORY: i8 = 1;
const KIND_SYMLINK: i8 = 2;
const BUNDLE_SIZE: u64 = 16777216;

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
    kind: i8,
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
/// Creates or updates an archive.
///
struct PackBuilder {
    // database connection
    conn: Connection,
    // byte offset within a bundle to which new content is added
    current_pos: u64,
    // item content that will reside in the bundle under construction
    contents: Vec<IncomingContent>,
    // workspace for compressing the content bundles
    buffer: Option<Vec<u8>>,
}

impl PackBuilder {
    ///
    /// Construct a new `PackBuilder` that will operate entirely in memory.
    ///
    fn new() -> Result<Self, Error> {
        let conn = Connection::open_in_memory()?;
        // can set the page_size when creating the database, but not after
        // conn.pragma_update(None, "page_size", 512)?;
        create_tables(&conn)?;
        Ok(Self {
            conn,
            current_pos: 0,
            contents: vec![],
            buffer: None,
        })
    }

    ///
    /// Visit all of the files and directories within the specified path, adding
    /// them to the database.
    ///
    /// **Note:** Remember to call `finish()` when done adding content.
    ///
    fn add_dir_all<P: AsRef<Path>>(&mut self, basepath: P) -> Result<u64, Error> {
        let mut file_count: u64 = 0;
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
    /// Call `finish()` when all file content has been added to the builder.
    ///
    /// The resulting database will be written to the given `path`.
    ///
    fn finish<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        if !self.contents.is_empty() {
            self.process_contents()?;
        }
        self.conn.backup(DatabaseName::Main, path, None)?;
        Ok(())
    }

    ///
    /// Process the current bundle of item content, clearing the collection and
    /// resetting the current content position.
    ///
    fn process_contents(&mut self) -> Result<(), Error> {
        self.insert_content()?;
        self.contents = vec![];
        self.current_pos = 0;
        Ok(())
    }

    ///
    /// Add a row to the `item` table that corresponds to this directory.
    ///
    fn add_directory<P: AsRef<Path>>(&self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, KIND_DIRECTORY, &name),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    ///
    /// Adds a single file to the archive, returning the item identifier.
    ///
    /// Depending on the size of the file and the content bundle so far, this
    /// may result in writing one or more rows to the content and itemcontent
    /// tables.
    ///
    /// **Note:** Remember to call `finish()` when done adding content.
    ///
    fn add_file<P: AsRef<Path>>(&mut self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, KIND_FILE, &name),
        )?;
        let item_id = self.conn.last_insert_rowid();
        let md = fs::metadata(path.as_ref());
        let file_len = match md.as_ref() {
            Ok(attr) => attr.len(),
            Err(_) => 0,
        };
        // empty files will result in an itemcontent row whose size is zero,
        // allowing for the extraction process to know to create an empty file
        // (otherwise it is difficult to tell from the available data)
        let mut itempos: u64 = 0;
        let mut size: u64 = file_len;
        loop {
            if self.current_pos + size > BUNDLE_SIZE {
                let remainder = BUNDLE_SIZE - self.current_pos;
                // add a portion of the file to fill the bundle
                let content = IncomingContent {
                    path: path.as_ref().to_path_buf(),
                    kind: KIND_FILE,
                    item: item_id,
                    itempos,
                    contentpos: self.current_pos,
                    size: remainder,
                };
                self.contents.push(content);
                // insert the content and itemcontent rows and start a new
                // bundle, then continue with the current file
                self.process_contents()?;
                size -= remainder;
                itempos += remainder;
            } else {
                // the remainder of the file fits within this content bundle
                let content = IncomingContent {
                    path: path.as_ref().to_path_buf(),
                    kind: KIND_FILE,
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

    ///
    /// Adds a symbolic link to the archive, returning the item identifier.
    ///
    /// **Note:** Remember to call `finish()` when done adding content.
    ///
    fn add_symlink<P: AsRef<Path>>(&mut self, path: P, parent: i64) -> Result<i64, Error> {
        let name = get_file_name(path.as_ref());
        self.conn.execute(
            "INSERT INTO item (parent, kind, name) VALUES (?1, ?2, ?3)",
            (&parent, KIND_SYMLINK, &name),
        )?;
        let item_id = self.conn.last_insert_rowid();
        let md = fs::symlink_metadata(path.as_ref());
        let link_len = match md.as_ref() {
            Ok(attr) => attr.len(),
            Err(_) => 0,
        };
        // assume that the link value is relatively small and simply add it into
        // the current content bundle in whole
        let content = IncomingContent {
            path: path.as_ref().to_path_buf(),
            kind: KIND_SYMLINK,
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
        let mut content: Vec<u8> = if let Some(mut buf) = self.buffer.take() {
            buf.clear();
            buf
        } else {
            Vec::with_capacity(BUNDLE_SIZE as usize)
        };
        let mut encoder = zstd::stream::write::Encoder::new(content, 0)?;

        // iterate through the file contents to build the compressed bundle
        for item in self.contents.iter() {
            if item.kind == KIND_FILE {
                let mut input = fs::File::open(&item.path)?;
                input.seek(SeekFrom::Start(item.itempos))?;
                let mut chunk = input.take(item.size);
                io::copy(&mut chunk, &mut encoder)?;
            } else if item.kind == KIND_SYMLINK {
                let value = read_link(&item.path)?;
                encoder.write_all(&value)?;
            }
        }
        content = encoder.finish()?;
        let compressed_len = content.len();

        // create space for the blob by inserting a zeroblob and then
        // overwriting it with the compressed content bundle
        //
        // NOTE: This insert takes the majority of the overall running time when
        // writing directly to disk.
        //
        self.conn.execute(
            "INSERT INTO content (value) VALUES (ZEROBLOB(?1))",
            [compressed_len as i32],
        )?;
        let content_id = self.conn.last_insert_rowid();
        let mut blob =
            self.conn
                .blob_open(DatabaseName::Main, "content", "value", content_id, false)?;
        let bytes_written = blob.write(&content)?;
        if bytes_written != content.len() {
            return Err(Error::IncompleteBlobWrite);
        }
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

///
/// Create a pack file at the given location and add all of the named inputs.
///
/// Returns the total number of files added to the archive.
///
fn create_archive<P: AsRef<Path>>(pack: P, inputs: Vec<&PathBuf>) -> Result<u64, Error> {
    let path_ref = pack.as_ref();
    let path = match path_ref.extension() {
        Some(_) => path_ref.to_path_buf(),
        None => path_ref.with_extension("db3"),
    };
    let mut builder = PackBuilder::new()?;
    let mut file_count: u64 = 0;
    for input in inputs {
        let metadata = input.metadata()?;
        if metadata.is_dir() {
            file_count += builder.add_dir_all(input)?;
        } else if metadata.is_file() {
            builder.add_file(input, 0)?;
            file_count += 1;
        }
    }
    builder.finish(path)?;
    Ok(file_count)
}

///
/// Return the last part of the path, converting to a String.
///
fn get_file_name<P: AsRef<Path>>(path: P) -> String {
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
fn read_link(path: &Path) -> Result<Vec<u8>, Error> {
    // convert whatever value returned by the OS into raw bytes without string conversion
    use os_str_bytes::OsStringBytes;
    let value = fs::read_link(path)?;
    Ok(value.into_os_string().into_raw_vec())
}

///
/// Create a symbolic link using the given raw bytes.
///
fn write_link(contents: &[u8], filepath: &Path) -> Result<(), Error> {
    use os_str_bytes::OsStringBytes;
    // this may panic if the bytes are not valid for this platform
    let target = std::ffi::OsString::from_io_vec(contents.to_owned())
        .ok_or_else(|| Error::LinkTextEncoding)?;
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
    return Ok(());
}

///
/// Reads the contents of an archive.
///
struct PackReader {
    conn: Connection,
}

impl PackReader {
    ///
    /// Construct a new `PackReader` that will read from the pack file at the
    /// given location.
    ///
    fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let conn = Connection::open(path.as_ref())?;
        Ok(Self { conn })
    }

    ///
    /// Return all items in the archive with the `name` as the full path.
    ///
    /// Directory entries have a path that ends with a slash (/).
    ///
    fn entries(&self) -> Result<Vec<Result<Entry, rusqlite::Error>>, Error> {
        //
        // Would love to return an iterator but that is quite difficult given
        // that the lifetimes and types are not very cooperative.
        //
        // Query from Pack in UPackDraft0Shared.pas that queries all items in
        // ascending order to make it easy to build the results.
        //
        let query = "WITH RECURSIVE FIT AS (
    SELECT *, Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
    UNION ALL
    SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
        FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
)
SELECT id, parent, kind, Path FROM FIT;";
        let mut stmt = self.conn.prepare(query)?;
        let items: Vec<Result<Entry, rusqlite::Error>> = stmt
            .query_map([], |row| {
                Ok(Entry {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    kind: row.get(2)?,
                    name: row.get(3)?,
                })
            })?
            .collect();
        Ok(items)
    }

    // Returns the number of files extracted.
    fn extract_all(&self) -> Result<u64, Error> {
        // ensure all of the directories are created, even empty ones
        self.ensure_all_directories()?;
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
        let mut item_iter = stmt.query_map([], |row| {
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
        while let Some(row_result) = item_iter.next() {
            let indexed_file = row_result?;
            if indexed_file.content != content_id {
                // reached the end of the entries for this content
                if !files.is_empty() {
                    file_count += self.process_content(files)?;
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
            file_count += self.process_content(files)?;
        }

        // clean up
        self.drop_temp_paths_table()?;
        Ok(file_count)
    }

    // Ensure that all directories in the archive are created, even those that
    // do not contain any files.
    fn ensure_all_directories(&self) -> Result<(), Error> {
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
            let fpath = pack_rs::sanitize_path(path)?;
            fs::create_dir_all(fpath)?;
        }
        Ok(())
    }

    // Process a single content blob and all of the files it contains.
    fn process_content(&self, files: Vec<IndexedFile>) -> Result<u64, Error> {
        assert!(!files.is_empty(), "expected files to be non-empty");
        let content_id = files[0].content;

        // fetch the blob and decompress
        let mut blob =
            self.conn
                .blob_open(DatabaseName::Main, "content", "value", content_id, true)?;
        let mut buffer: Vec<u8> = Vec::new();
        zstd::stream::copy_decode(&mut blob, &mut buffer)?;

        // process each of the rows of content, which are portions of a file
        let mut file_count: u64 = 0;
        for entry in files.iter() {
            // perform basic sanitization of the file path to prevent abuse (it
            // is theoretically possible that the data could produce a path with
            // a root, prefix, parent-dir elements)
            let fpath = pack_rs::sanitize_path(&entry.path)?;
            if entry.kind == KIND_FILE {
                // make sure the file exists and is writable
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(&fpath)?;
                let file_len = fs::metadata(fpath)?.len();
                if file_len == 0 {
                    // just created a new file, count it
                    file_count += 1;
                }
                // if the file was an empty file, then we are already done here
                if entry.size > 0 {
                    // ensure the file has the appropriate length for writing this
                    // content chunk into the file, extending it as necessary
                    if file_len < entry.itempos {
                        output.set_len(entry.itempos)?;
                    }
                    // seek to the correct position within the file for this chunk
                    if entry.itempos > 0 {
                        output.seek(SeekFrom::Start(entry.itempos))?;
                    }
                    // use Cursor because that's seemingly easier than getting a slice
                    let mut cursor = std::io::Cursor::new(&buffer);
                    cursor.seek(SeekFrom::Start(entry.contentpos))?;
                    let mut chunk = cursor.take(entry.size);
                    io::copy(&mut chunk, &mut output)?;
                }
            } else if entry.kind == KIND_SYMLINK {
                // use Cursor because that's seemingly easier than getting a slice
                let mut cursor = std::io::Cursor::new(&buffer);
                cursor.seek(SeekFrom::Start(entry.contentpos))?;
                let mut chunk = cursor.take(entry.size);
                let mut raw_bytes: Vec<u8> = vec![];
                chunk.read_to_end(&mut raw_bytes)?;
                write_link(&raw_bytes, &fpath)?;
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

    // returns 0 if file not found
    #[allow(dead_code)]
    fn find_file_by_path(&self, relpath: &str) -> Result<i64, Error> {
        let sql = format!(
            "WITH RECURSIVE IT AS (
    SELECT Item.*, ID AS FID FROM Item WHERE
    ID IN (
        WITH RECURSIVE FIT AS (
            SELECT *, '/' || Name || IIF(Kind = 1, '/', '') AS Path FROM Item WHERE Parent = 0
            UNION ALL
            SELECT Item.*, FIT.Path || Item.Name || IIF(Item.Kind = 1, '/', '') AS Path
                FROM Item INNER JOIN FIT ON FIT.Kind = 1 AND Item.Parent = FIT.ID
                WHERE '/{}' LIKE (Path || '%')
        )
        SELECT ID FROM FIT WHERE Path IN ('/{}')
    )
    UNION ALL
    SELECT Item.*, IT.FID FROM Item INNER JOIN IT ON IT.Kind = 1 AND Item.Parent = IT.ID
),
ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY FID, ID) - 1) AS I, * FROM IT)
SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
LEFT JOIN ITI AS P ON C.FID = P.FID AND C.Parent = P.ID ORDER BY C.I;",
            relpath, relpath
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let item_iter = stmt.query_map([], |row| {
            Ok(Entry {
                id: row.get(2)?,
                parent: row.get(3)?,
                kind: row.get(4)?,
                name: row.get(5)?,
            })
        })?;
        for entry in item_iter {
            return Ok(entry?.id);
        }
        Ok(0)
    }

    //
    // Print the contents of the identified file to stdout.
    //
    #[allow(dead_code)]
    fn print_file(&self, item_id: i64) -> Result<(), Error> {
        let mut stmt = self.conn.prepare(
            "SELECT content, contentpos, size FROM itemcontent WHERE item = ?1 ORDER BY itempos",
        )?;
        let content_iter = stmt.query_map([&item_id], |row| {
            Ok(OutgoingContent {
                content: row.get(0)?,
                contentpos: row.get(1)?,
                size: row.get(2)?,
            })
        })?;
        for content_result in content_iter {
            let itemcontent = content_result?;
            let mut blob = self.conn.blob_open(
                DatabaseName::Main,
                "content",
                "value",
                itemcontent.content,
                true,
            )?;
            let mut buffer: Vec<u8> = Vec::new();
            let mut output = io::stdout();
            zstd::stream::copy_decode(&mut blob, &mut buffer)?;
            // use Cursor because that's seemingly easier than getting a slice
            let mut cursor = std::io::Cursor::new(buffer);
            cursor.seek(SeekFrom::Start(itemcontent.contentpos))?;
            let mut chunk = cursor.take(itemcontent.size);
            io::copy(&mut chunk, &mut output)?;
        }
        Ok(())
    }
}

///
/// List all file entries in the archive in breadth-first order.
///
fn list_contents(pack: &str) -> Result<(), Error> {
    if !pack_rs::is_pack_file(pack)? {
        return Err(Error::NotPackFile);
    }
    let reader = PackReader::new(pack)?;
    let entries = reader.entries()?;
    for result in entries {
        let entry = result?;
        if entry.kind != KIND_DIRECTORY {
            println!("{}", entry.name)
        }
    }
    Ok(())
}

///
/// Extract all of the files from the archive.
///
fn extract_contents(pack: &str) -> Result<u64, Error> {
    if !pack_rs::is_pack_file(pack)? {
        return Err(Error::NotPackFile);
    }
    let reader = PackReader::new(pack)?;
    let file_count = reader.extract_all()?;
    Ok(file_count)
}

///
/// `Entry` represents a row from the `item` table.
///
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub parent: i64,
    pub kind: i8,
    pub name: String,
}

// Result from the IndexedFiles temporary table joined with itemcontent table.
#[derive(Debug)]
struct IndexedFile {
    content: i64,
    contentpos: u64,
    itempos: u64,
    size: u64,
    kind: i8,
    path: String,
}

struct OutgoingContent {
    // rowid of the content in the content table
    content: i64,
    // offset within the content bundle where the data will go
    contentpos: u64,
    // size of the item content
    size: u64,
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
