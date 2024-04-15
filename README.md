# pack-rs

An on-going effort to make something like [Pack](https://pack.ac) using [Rust](https://www.rust-lang.org). Pack is an archiver/compressor that takes a novel approach to the problem that has largely been dominated by two formats for the past couple of decades, tar/gz and zip. Original idea by [O](https://github.com/OttoCoddo) with an implementation in [Pascal](https://github.com/PackOrganization/Pack) and some very clever SQL.

## Objectives

Build both a Rust crate and a binary that can be used to produce fairly small archives of a collection of files. The crate will have an interface similar to that of the [tar](https://crates.io/crates/tar) crate.

Short term features:

* Build an archive concurrently using multiple threads; same for extract.
* Size the content bundles dynamically based on the incoming data size.
* Set the database page size dynamically based on the incoming data size.
* Add a new file to an existing archive.
* Remove a file from an archive.
* Support include/exclude patterns when building or extracting an archive.
* Store symbolic links (currently ignored).

In the long term, there are additional features that would be nice-to-have:

* Optional compression of a pack using any available algorithm, not just Zstandard. Currently, the Rust `zstd` crate lacks a nice way of detecting if a block of data is compressed using Zstandard. What's more, it would be ideal to future-proof the design by allowing an implementation to use any compression algorithm. Likely would add a `TEXT` column to the `content` table that specifies the compression algorithm used (e.g. `7zip`).
* Support encryption of the content blobs (will require compression, probably involve a salt value stored in the `content` table).
* Optionally store file metadata (owners, permissions, etc) in a separate table.
* Optionally store file extended attributes in a separate table.

## Build and Run

### Prerequisites

* [Rust](https://www.rust-lang.org) 2021 edition

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

Rows in the `item` table represent both directories and files. The `kind` for files is `0` and the `kind` for directories is `1`. The `name` is the final part of the file path, such as `README.md` or `src`. The `parent` refers to the directory that contains this entry on the file system, with `0` indicating the entry is at the "root" of the archive.

| Name     | Type                  | Description        |
| -------- | --------------------- | ------------------ |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the item |
| `parent` | `INTEGER`             | rowid in the `item` table for the directory that contains this |
| `kind`   | `INTEGER`             | either `0` (file) or `1` (directory) |
| `name`   | `TEXT NOT NULL`       | name of the directory or file |

### content

Rows in the `content` table are nothing more than huge blobs of compressed data that contain the file data within the archive. The size of these blobs can vary, anywhere from 8 to 32 MiB (mebibytes) with the idea being that larger blocks of contiguous content will compress better.

| Name     | Type                  | Description               |
| -------- | --------------------- | ------------------------- |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the content     |
| `value`  | `BLOB`                | (compressed) file content |

The content blobs are built up from the contents of as many files as it takes to fill the target blob size, at which point the entire block is compressed using Zstandard (without a dictionary). How the file contents are mapped to the content blobs is defined in the `itemcontent` table described below.

### itemcontent

The `itemcontent` table is the glue that binds the rows from the `item` table to the huge blobs in the `content` table. Typically a blob is large enough to hold many files, as such this table will show which blob contains the data for a particular file, and where within the (decompressed) blob to read the data.

For very large files that are larger than the blob size, they will reference multiple rows from the `content` table. The `itempos` and `contentpos` values make it possible to accommodate both small files that fit within a blob and large files that do not.

**Note:** empty files will have a row in the `itemcontent` table with a `size` of zero to make it easier to write the extraction implementation.

| Name         | Type                  | Description               |
| ------------ | --------------------- | ------------------------- |
| `id`         | `INTEGER PRIMARY KEY` | rowid for the itemcontent |
| `item`       | `INTEGER`             | rowid in the `item` table for the file |
| `itempos`    | `INTEGER`             | position within the file for this chunk of content |
| `content`    | `INTEGER`             | rowid in the `content` table where this chunk is stored |
| `contentpos` | `INTEGER`             | position within the chunk from the `content` table for this chunk |
| `size`       | `INTEGER`             | the size of the chunk |

## Pros and Cons

### Pros

* The original [Pack](https://pack.ac), and hopefully this one too, are very fast and produce fairly small archives.
* The container format, [SQLite](https://www.sqlite.org) is small, fast, and reliable.
* By virtue of using a database, accessing individual file content is very fast.

### Cons

* Not well suited to very small data sets. The overhead of the database will outweigh anything less than about 20 KB.
* Streaming input and output, a la tar or gzip, is not feasible with this design.
