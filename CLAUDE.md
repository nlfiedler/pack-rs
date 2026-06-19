# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`pack-rs` is an experimental Rust archiver/compressor that stores archives as
SQLite databases with file data held in large Zstandard-compressed blobs. It is
a reimplementation of the ideas from [Pack](https://pack.ac) (original in
Pascal). The project is early-stage and explicitly a prototype; the long-term
goal (see `TODO.org`) is to expose a library API resembling the `tar` crate.

## Commands

```shell
cargo build                 # build
cargo test                  # run all tests (currently just lib.rs unit tests)
cargo test test_sanitize_path   # run a single test by name
cargo run -- create pack.db3 <paths...>   # create an archive
cargo run -- list pack.db3                # list file entries
cargo run -- extract pack.db3             # extract into the current directory
```

Subcommands have short flags: `create`/`-c`, `list`/`-l`, `extract`/`-x`. If the
output path passed to `create` has no extension, `.db3` is appended.

### Prerequisites

Needs a C/C++ toolchain and the SQLite 3 dev library on Unix (`libsqlite3-dev`,
`sqlite-devel`, or `brew install sqlite`). On Windows, SQLite is bundled via the
`rusqlite` `bundled` feature (see the per-target dependency split in
`Cargo.toml`).

## Architecture

The archive format is three SQLite tables (full schema in `README.md`, sample
queries in `doc/internal/queries.sql`):

- **`item`** — one row per directory/file/symlink. `kind` is `0`=file,
  `1`=directory, `2`=symlink (constants `KIND_*` in `main.rs`). `parent` points
  at the containing directory's rowid, with `0` meaning archive root. Only the
  final path component is stored in `name`; full paths are reconstructed at read
  time via a recursive CTE.
- **`content`** — large compressed blobs (`BUNDLE_SIZE` = 16 MiB target). Many
  small files are packed into one blob before compression so contiguous data
  compresses better; a single large file spans multiple blobs.
- **`itemcontent`** — the mapping layer: which blob holds a file's data and at
  what offsets. `itempos` is the offset within the file, `contentpos` the offset
  within the decompressed blob, `size` the chunk length. Empty files get a row
  with `size = 0` so extraction can recreate them.

Two driving types in `src/main.rs`:

- **`PackBuilder`** (write path) — walks the input tree breadth-first
  (`add_dir_all`), accumulating `IncomingContent` entries until a bundle reaches
  `BUNDLE_SIZE`, then `insert_content` compresses the bundle and writes the
  `content` + `itemcontent` rows. Symlink targets are stored as raw bytes, like
  file content.
- **`PackReader`** (read path) — `entries` lists paths via a recursive CTE;
  `extract_all` builds a temporary `IndexedFiles` table joining item paths to
  `itemcontent`, then processes blobs in `content`/`contentpos` order so each
  blob is decompressed exactly once.

`src/lib.rs` holds the public surface: the `Error` enum (`thiserror`),
`is_pack_file` (validates the SQLite header bytes and that an `item` table
exists), and `sanitize_path` (strips roots/prefixes/`..` to prevent extraction
escaping the target directory — always used before writing extracted files).

### Key implementation notes

- **In-memory then backup-to-disk.** `PackBuilder::new` opens an in-memory
  SQLite connection and `finish` calls `Connection::backup` to write it out.
  This is a deliberate performance workaround: writing blobs directly to an
  on-disk database spends ~90% of runtime in the `ZEROBLOB` allocation insert.
  The code was previously multi-threaded but is currently single-threaded and
  in-memory (the `ThreadPoolShutdown` error variant is a leftover from that).
- **Blob writes** use the `INSERT ... ZEROBLOB(n)` + `blob_open`/`write` pattern
  rather than binding the bytes directly.
- **Path/symlink byte handling.** Symlink targets are read/written as raw OS
  bytes via `os_str_bytes` (`read_link`/`write_link`) to avoid lossy UTF-8
  conversion; `write_link` is `cfg`-gated for unix vs windows.
- The recursive CTE for path reconstruction is duplicated in several methods
  (`entries`, `ensure_all_directories`, `create_temp_paths_table`); changes to
  path semantics must be kept in sync across them.

The schema intentionally differs slightly from upstream Pack but is functionally
compatible. Planned work (existing-archive updates, file attributes/xattrs
tables, encryption, include/exclude filters, page-size tuning) is tracked in
`TODO.org`.
