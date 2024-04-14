# pack-rs

An experiment to make something like [Pack](https://pack.ac) using [Rust](https://www.rust-lang.org). Pack is an archiver/compressor that takes a novel approach to the problem that has largely been dominated by two formats for the past couple of decades, tar/gz and zip. Original idea by [O](https://github.com/OttoCoddo) with an implementation in [Pascal](https://github.com/PackOrganization/Pack) and a lot of very clever SQL.

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

Long term, additional features would be nice-to-have:

* Optional compression of a pack with any available algorithm, not just Zstandard. Currently, the Rust `zstd` crate lacks a nice way of detecting if a block of data is compressed using Zstandard. What's more, it would be ideal to future-proof the design by allowing an implementation to use any compression algorithm. Likely would add a `TEXT` column to the `content` table that specifies the compression algorithm used (e.g. `7zip`).
* Support encryption of the content blobs (will require compression, probably involve a salt value stored in the `content` table).
* Optionally store file metadata (owners, permissions, etc) in a separate table.
* Optionally store file extended attributes in a separate table.

## Build and Run

### Prerequisites

* [Rust](https://www.rust-lang.org) 2021 edition

### Instructions

For the time being there are very few unit tests.

```shell
cargo test
```

Invoking `cargo run` will attempt to read from a directory that you will almost certainly not have on your system, resulting in an error. For now, modify `main.rs` to read from a suitable location when building the archive. This will be fixed very soon.

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

| Name         | Type                  | Description               |
| ------------ | --------------------- | ------------------------- |
| `id`         | `INTEGER PRIMARY KEY` | rowid for the itemcontent |
| `item`       | `INTEGER`             | rowid in the `item` table for the file |
| `itempos`    | `INTEGER`             | position within the file for this chunk of content |
| `content`    | `INTEGER`             | rowid in the `content` table where this chunk is stored |
| `contentpos` | `INTEGER`             | position within the chunk from the `content` table for this chunk |
| `size`       | `INTEGER`             | the size of the chunk |
