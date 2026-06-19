# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`pack-rs` is an experimental Rust archiver/compressor that stores archives as
SQLite databases with file data held in large Zstandard-compressed blobs. It is
a reimplementation of the ideas from [Pack](https://pack.ac) (original in
Pascal). It is a `lib` + `bin` crate: the library exposes a `tar`-like API
(`Builder` / `Archive`) and the CLI is a thin client of it.

## Commands

```shell
cargo build                 # build
cargo test                  # run unit + integration tests (tests/roundtrip.rs)
cargo test test_sanitize_path   # run a single test by name
cargo clippy --all-targets  # lint (kept clean)
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

## Module layout

`src/lib.rs` is the crate root: it declares the modules and re-exports the
public API (`Builder`, `Archive`, `Entry`, `Kind`, `Error`, plus the
`is_pack_file` / `sanitize_path` / `symlink_target_within_root` helpers).

- `src/builder.rs` — `Builder` (write path) + private `IncomingContent`, table
  creation, and the bundle/compression options.
- `src/archive.rs` — `Archive` (read path) + public `Entry` and private
  `IndexedFile`.
- `src/error.rs` — the `Error` enum (`thiserror`).
- `src/kind.rs` — `Kind { File, Directory, Symlink }` with `as_i64` /
  `from_i64`, replacing the old `KIND_*` integer constants. SQL keeps the
  numeric literals (`0/1/2`); only Rust code uses `Kind`.
- `src/util.rs` — path/link helpers: public `is_pack_file`, `sanitize_path`,
  `symlink_target_within_root`, and crate-internal `get_file_name`, `read_link`,
  `decode_link`, `write_link`, `verify_within_root`.
- `src/main.rs` — thin CLI (clap) calling the library.

## Architecture

The archive format is three SQLite tables (full schema in `README.md`, sample
queries in `doc/internal/queries.sql`):

- **`item`** — one row per directory/file/symlink. `kind` is `0`=file,
  `1`=directory, `2`=symlink (the `Kind` enum in `src/kind.rs`). `parent` points
  at the containing directory's rowid, with `0` meaning archive root. Only the
  final path component is stored in `name`; full paths are reconstructed at read
  time via a recursive CTE.
- **`content`** — large compressed blobs (default 16 MiB target, configurable via
  `Builder::bundle_size`). Many small files are packed into one blob before
  compression so contiguous data compresses better; a single large file spans
  multiple blobs.
- **`itemcontent`** — the mapping layer: which blob holds a file's data and at
  what offsets. `itempos` is the offset within the file, `contentpos` the offset
  within the decompressed blob, `size` the chunk length. Empty files get a row
  with `size = 0` so extraction can recreate them.

The two driving types:

- **`Builder`** (`src/builder.rs`) — `append_dir_all` walks the input tree
  depth-first (using a stack), accumulating `IncomingContent` entries until a
  bundle reaches `bundle_size`, then `insert_content` compresses the bundle and
  writes the `content` + `itemcontent` rows. `finish(dest)` backs the in-memory
  db up to disk. Symlink targets are stored as raw bytes, like file content.
- **`Archive`** (`src/archive.rs`) — `entries` lists paths via a recursive CTE;
  `unpack(dest)` builds a temporary `IndexedFiles` table joining item paths to
  `itemcontent`, then processes blobs in `content`/`contentpos` order so each
  blob is decompressed exactly once, writing every output under `dest`.

### Key implementation notes

- **In-memory then backup-to-disk.** `Builder::new` opens an in-memory SQLite
  connection and `finish` calls `Connection::backup` to write it out. This is a
  deliberate performance workaround: writing blobs directly to an on-disk
  database spends ~90% of runtime in the `ZEROBLOB` allocation insert.
- **Extraction is destination-rooted and sandboxed.** `unpack(dest)`
  canonicalizes `dest` as the root and writes `root.join(sanitize_path(path))`;
  `verify_within_root` (parent canonicalization) and `symlink_target_within_root`
  (lexical target check) refuse any entry that would escape `dest`. The CLI's
  `extract` passes `"."`.
- **Blob writes** use the `INSERT ... ZEROBLOB(n)` + `blob_open`/`write` pattern
  rather than binding the bytes directly.
- **Path/symlink byte handling.** Symlink targets are read/written as raw OS
  bytes via `os_str_bytes` (`read_link`/`write_link`) to avoid lossy UTF-8
  conversion; `write_link` is `cfg`-gated for unix vs windows.
- The recursive CTE for path reconstruction is duplicated across `Archive`
  methods (`entries`, `ensure_all_directories`, `create_temp_paths_table`);
  changes to path semantics must be kept in sync across them.

The schema intentionally differs slightly from upstream Pack but is functionally
compatible. Planned work (existing-archive updates, file attributes/xattrs
tables, encryption, include/exclude filters, page-size tuning) is tracked in
`TODO.org`.
