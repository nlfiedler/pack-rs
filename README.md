# pack-rs

An experiment to make something like [Pack](https://pack.ac) using [Rust](https://www.rust-lang.org). Pack is an archiver/compressor that takes a novel approach to the problem that has largely been dominated by two formats for the past couple of decades, tar/gz and zip.

## Objectives

Build both a Rust crate and a binary that can be used to produce fairly small archives of a collection of files. The crate will have an interface similar to that of the [tar](https://crates.io/crates/tar) crate.

## Build and Run

### Prerequisites

* [Rust](https://www.rust-lang.org) 2021 edition

### Instructions

For the time being there are no unit tests, so simply build and run like so:

```shell
cargo run
```

## Specification

A pack file is an [SQLite](https://www.sqlite.org) database with file data stored in large blobs compressed using [Zstandard](http://facebook.github.io/zstd/). There are three primary tables.

### item

Rows in the `item` table represent both directories and files. The `kind` for directories is `0` and the `kind` for files is `1`. The `name` is the final part of the file path, such as `README.md` or `src`. The `parent` refers to the directory that contains this entry on the file system, with `0` indicating the entry is at the "root" of the archive.

| Name     | Type                  | Description        |
| -------- | --------------------- | ------------------ |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the item |
| `parent` | `INTEGER`             | rowid in the `item` table for the directory that contains this |
| `kind`   | `INTEGER`             | either `0` (directory) or `1` (file) |
| `name`   | `TEXT NOT NULL`       | name of the directory or file |

### content

Rows in the `content` table are nothing more than huge blobs of compressed data that contain the file data within the archive. The size of these blobs can vary, anywhere from 8 to 32 MiB (mebibytes) with the idea being that larger blocks of continguous content will compress better.

| Name     | Type                  | Description               |
| -------- | --------------------- | ------------------------- |
| `id`     | `INTEGER PRIMARY KEY` | rowid for the content     |
| `value`  | `BLOB`                | (compressed) file content |

The content blobs are built up from the contents of as many files as it takes to fill the target blob size, at which point the entire block is compressed using Zstandard (without a dictionary). How the file contents are mapped to the content blobs is defined in the `itemcontent` table described below.

### itemcontent

The `itemcontent` table is the glue that binds the rows from the `item` table to the huge blobs in the `content` table. Typically a blob is large enough to hold many files, as such this table will show which blob contains the data for a particular file, and where within the (decompressed) blob to read the data.

For very large files that are larger than the blob size, they will reference multiple rows from the `content` table. The `itempos` and `contentpos` values make it possible to accomodate both small files that fit within a blob and large files that do not.

| Name         | Type                  | Description               |
| ------------ | --------------------- | ------------------------- |
| `id`         | `INTEGER PRIMARY KEY` | rowid for the itemcontent |
| `item`       | `INTEGER`             | rowid in the `item` table for the file |
| `itempos`    | `INTEGER`             | position within the file for this chunk of content |
| `content`    | `INTEGER`             | rowid in the `content` table where this chunk is stored |
| `contentpos` | `INTEGER`             | position within the chunk from the `content` table for this chunk |
| `size`       | `INTEGER`             | the size of the chunk |
