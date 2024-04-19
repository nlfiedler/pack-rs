# pack-rs

An experiment to make something like [Pack](https://pack.ac) using [Rust](https://www.rust-lang.org). Pack is an archiver/compressor that takes a novel approach to the problem that has largely been dominated by two formats for the past couple of decades, tar/gz and zip. Original idea by [O](https://github.com/OttoCoddo) with an implementation in [Pascal](https://github.com/PackOrganization/Pack) and some very clever SQL.

## Status

This was an experiment, and a very interesting one at that. If and when the original Pack has a specification, and other people take an interest in it, then I would be more than happy to continue working on this project. All of the objectives that I had in mind are written in the `TODO.org` file (an emacs [org-mode](https://orgmode.org) file), including building a Rust crate with an API similar to that of [tar](https://crates.io/crates/tar).

## Build and Run

### Prerequisites

* [Rust](https://www.rust-lang.org) 2021 edition
* C/C++ toolchain:
    - macOS: [Clang](https://clang.llvm.org) probably?
    - RockyLinux: `sudo yum install gcc-c++ make`
    - Ubuntu: `sudo apt-get install build-essential`
    - Windows: [MSVC](https://visualstudio.microsoft.com/visual-cpp-build-tools/) build tools and Windows SDK
* SQLite 3 library:
    - macOS: `brew install sqlite`
    - RockyLinux: `sudo yum install sqlite-devel`
    - Ubuntu: `sudo apt-get install libsqlite3-dev`
    - Windows: _it will be bundled with rusqlite_

### Running the tests

For the time being there are very few unit tests.

```shell
cargo test
```

### Creating, listing, extracting archives

Start by creating an archive using the `create` subcommand. The example below assumes that you have downloaded something interesting into your `~/Downloads` directory.

```shell
$ cargo run -- create pack.db3 ~/Downloads/httpd-2.4.59
...
Added 3138 files to pack.db3
```

Now that the `pack.db3` file exists, you can list the contents like so:

```shell
$ cargo run -- list pack.db3 | head -20
...
httpd-2.4.59/.deps
httpd-2.4.59/.gdbinit
httpd-2.4.59/.gitignore
httpd-2.4.59/ABOUT_APACHE
httpd-2.4.59/Apache-apr2.dsw
httpd-2.4.59/Apache.dsw
httpd-2.4.59/BuildAll.dsp
httpd-2.4.59/BuildBin.dsp
...
```

Finally, run `extract` to unpack the contents of the archive into the current directory:

```shell
$ cargo run -- extract pack.db3
...
Extracted 3138 files from pack.db3
```

## Specification

A pack file is an [SQLite](https://www.sqlite.org) database with file data stored in large blobs compressed using [Zstandard](http://facebook.github.io/zstd/). There are three primary tables.

**Note:** The schema described here differs slightly from [Pack](https://pack.ac) but is largely the same for all intents and purposes.

### item

Rows in the `item` table represent directories, files, and symbolic links. The `kind` for files is `0`, the `kind` for directories is `1`, and the `kind` for symbolic links is `2`. The `name` is the final part of the file path, such as `README.md` or `src`. The `parent` refers to the directory that contains this entry on the file system, with `0` indicating the entry is at the "root" of the archive.

| Name     | Type                  | Description        |
| -------- | --------------------- | ------------------ |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the item |
| `parent` | `INTEGER`             | rowid in the `item` table for the directory that contains this |
| `kind`   | `INTEGER`             | `0` (file), `1` (directory), `2` (symlink) |
| `name`   | `TEXT NOT NULL`       | name of the directory or file |

### content

Rows in the `content` table are nothing more than huge blobs of compressed data that contain the file data within the archive. The size of these blobs can vary, anywhere from 8 to 32 MiB (mebibytes) with the idea being that larger blocks of contiguous content will compress better.

| Name     | Type                  | Description               |
| -------- | --------------------- | ------------------------- |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the content     |
| `value`  | `BLOB`                | (compressed) file content |

The content blobs are built up from the contents of as many files as it takes to fill the target blob size, at which point the entire block is compressed using Zstandard (without a dictionary). How the file contents are mapped to the content blobs is defined in the `itemcontent` table described below.

For symbolic links, the raw bytes are stored as if they were file content.

### itemcontent

The `itemcontent` table is the glue that binds the rows from the `item` table to the huge blobs in the `content` table. Typically a blob is large enough to hold many files, as such this table will show which blob contains the data for a particular file, and where within the (decompressed) blob to read the data.

For very large files that are larger than the blob size, they will reference multiple rows from the `content` table. The `itempos` and `contentpos` values make it possible to accommodate both small files that fit within a blob and large files that do not.

Empty files will have a row in the `itemcontent` table with a `size` of zero to make it easier to write the extraction implementation.

| Name         | Type                  | Description               |
| ------------ | --------------------- | ------------------------- |
| `id`         | `INTEGER PRIMARY KEY` | rowid for the itemcontent |
| `item`       | `INTEGER`             | rowid in the `item` table for the file |
| `itempos`    | `INTEGER`             | position within the file for this chunk of content |
| `content`    | `INTEGER`             | rowid in the `content` table where this chunk is stored |
| `contentpos` | `INTEGER`             | position within the chunk from the `content` table for this chunk |
| `size`       | `INTEGER`             | the size of the chunk |

## Performance Considerations

When writing to a database file on secondary storage, the majority of the running time (~90%) is spent in the allocation of the blob in SQLite using this statement:

```rust
conn.execute(
    "INSERT INTO content (value) VALUES (ZEROBLOB(?1))",
    [compressed_len as i32],
)?;
```

This may be a limitation in the current API of the `rusqlite` crate, or due to my lack of expertise in the usage of SQLite. As a work-around, the program creates an in-memory database and writes to disk when finished.
