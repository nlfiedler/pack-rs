//
// Copyright (c) 2024 Nathan Fiedler
//
use rusqlite::blob::ZeroBlob;
use rusqlite::{Connection, DatabaseName};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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
}

const KIND_DIRECTORY: i8 = 0;
const KIND_FILE: i8 = 1;
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
    // the rowid in the item table
    item: i64,
    // offset within the file from which to start, usually zero
    itempos: u64,
    // offset within the content bundle where the data will go
    contentpos: u64,
    // size of the item content
    size: u64,
}

//
// Holds information about a virtual content bundle and the file portions that
// will be written to this bundle when it is committed to the database.
//
struct ContentBundle {
    // byte offset within the bundle to which new content is added
    current_pos: u64,
    // list of item content that will reside in this bundle
    contents: Vec<IncomingContent>,
}

///
/// Creates or updates an archive.
///
struct PackBuilder {
    // database connection
    conn: Connection,
    // data on the file portions that constitute a virtual content bundle
    bundle: ContentBundle,
}

impl PackBuilder {
    ///
    /// Construct a new `PackBuilder` that will create or update the pack file
    /// at the given location.
    ///
    fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let conn = Connection::open(path.as_ref())?;
        let bundle = ContentBundle {
            current_pos: 0,
            contents: vec![],
        };
        // can set the page_size when creating the database, but not after
        // conn.pragma_update(None, "page_size", 512)?;
        create_tables(&conn)?;
        Ok(Self { conn, bundle })
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
                }
            }
        }
        Ok(file_count)
    }

    ///
    /// Call `finish()` when all file content has been added to the builder.
    ///
    fn finish(&mut self) -> Result<(), Error> {
        if !self.bundle.contents.is_empty() {
            self.insert_content()?;
        }
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
        let md = fs::symlink_metadata(path.as_ref());
        let file_len = match md.as_ref() {
            Ok(attr) => attr.len(),
            Err(_) => 0,
        };
        // empty files will not result in any new itemcontent rows
        let mut itempos: u64 = 0;
        let mut size: u64 = file_len;
        while size > 0 {
            if self.bundle.current_pos + size > BUNDLE_SIZE {
                let remainder = BUNDLE_SIZE - self.bundle.current_pos;
                // add a portion of the file to fill the bundle
                let content = IncomingContent {
                    path: path.as_ref().to_path_buf(),
                    item: item_id,
                    itempos,
                    contentpos: self.bundle.current_pos,
                    size: remainder,
                };
                self.bundle.contents.push(content);
                // insert the content and itemcontent rows
                self.insert_content()?;
                // start a new bundle and continue with the current file
                self.bundle.current_pos = 0;
                self.bundle.contents = vec![];
                size -= remainder;
                itempos += remainder;
            } else {
                // the remainder of the file fits within this content bundle
                let content = IncomingContent {
                    path: path.as_ref().to_path_buf(),
                    item: item_id,
                    itempos,
                    contentpos: self.bundle.current_pos,
                    size,
                };
                self.bundle.contents.push(content);
                self.bundle.current_pos += size;
                size = 0;
            }
        }
        Ok(item_id)
    }

    //
    // Creates a content bundle based on the data collected so far, then
    // compresses it, writing the blob to a new row in the `content` table. Then
    // creates the necessary rows in the `itemcontent` table to map the file
    // data to the content bundle.
    //
    fn insert_content(&mut self) -> Result<(), Error> {
        // Set bundle capacity to some estimate of expected size with half of
        // the total size being a rough estimate, since the file data will be
        // compressed on the way in; worst case the vector will reallocate to
        // twice the size once or twice instead of many times.
        let total_size: Option<u64> = self
            .bundle
            .contents
            .iter()
            .map(|e| e.size)
            .reduce(|acc, e| acc + e);
        let mut content: Vec<u8> = match total_size {
            Some(size) => Vec::with_capacity((size / 2) as usize),
            None => Vec::new(),
        };
        let mut encoder = zstd::stream::write::Encoder::new(content, 0)?;

        // iterate through the file contents to build the compressed bundle
        for item in self.bundle.contents.iter() {
            let mut input = fs::File::open(&item.path)?;
            input.seek(SeekFrom::Start(item.itempos))?;
            let mut chunk = input.take(item.size);
            io::copy(&mut chunk, &mut encoder)?;
        }
        content = encoder.finish()?;
        let compressed_len = content.len();

        // create space for the blob by inserting a zeroblob and then
        // overwriting it with the compressed content bundle
        self.conn.execute(
            "INSERT INTO content (value) VALUES (?1)",
            [ZeroBlob(compressed_len as i32)],
        )?;
        let content_id = self.conn.last_insert_rowid();
        let mut blob =
            self.conn
                .blob_open(DatabaseName::Main, "content", "value", content_id, false)?;
        let bytes_written = blob.write(&content)?;
        if bytes_written != content.len() {
            return Err(Error::IncompleteBlobWrite);
        }

        // iterate through the item contents and insert new itemcontent rows
        for item in self.bundle.contents.iter() {
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
    /// Return all items in the archive.
    ///
    fn entries(&self) -> Result<Vec<Result<Entry, rusqlite::Error>>, Error> {
        //
        // Would love to return an iterator but that is quite difficult given
        // that the lifetimes and types are not very cooperative.
        //
        // Query from Pack in UPackDraft0Shared.pas that queries all items in
        // ascending order to make it easy to build the results.
        //
        let query = "WITH IT AS (SELECT * FROM Item),
            ITI AS (SELECT (ROW_NUMBER() OVER (ORDER BY ID) - 1) AS I, * FROM IT)
            SELECT C.I, IFNULL(P.I, -1) AS PI, C.ID, C.Parent, C.Kind, C.Name FROM ITI AS C
            LEFT JOIN ITI AS P ON C.Parent = P.ID ORDER BY C.I";
        let mut stmt = self.conn.prepare(query)?;
        let items: Vec<Result<Entry, rusqlite::Error>> = stmt
            .query_map([], |row| {
                // rows: 0: I, 1: PI, 2: id, 3: parent, 4: kind, 5: name
                Ok(Entry {
                    id: row.get(2)?,
                    parent: row.get(3)?,
                    kind: row.get(4)?,
                    name: row.get(5)?,
                })
            })?
            .collect();
        Ok(items)
    }

    //
    // Print the contents of the identified file to stdout.
    //
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
/// `Entry` represents a row from the `item` table.
///
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub parent: i64,
    pub kind: i64,
    pub name: String,
}

struct OutgoingContent {
    // rowid of the content in the content table
    content: i64,
    // offset within the content bundle where the data will go
    contentpos: u64,
    // size of the item content
    size: u64,
}

fn main() -> Result<(), Error> {
    let path = "./pack.db3";
    let inpath = Path::new("src");
    let mut builder = PackBuilder::new(path)?;
    let file_count = builder.add_dir_all(inpath)?;
    builder.finish()?;
    println!("added {} files", file_count);

    // list all entries in the archive in a sensible order
    let reader = PackReader::new(path)?;
    let entries = reader.entries()?;
    for entry in entries {
        println!("entry: {:?}", entry)
    }

    // src/main.rs
    reader.print_file(2)?;
    Ok(())
}
