# Third-party notices

Builds of the toolkit compile in source code from these projects through
their Rust bindings. Their licenses apply to the compiled code they
contribute.

| Component | Pulled in by | License |
| --- | --- | --- |
| [SQLite](https://sqlite.org/) | `rusqlite` with the `bundled` feature (`libsqlite3-sys`) | Public domain |
| [Zstandard](https://github.com/facebook/zstd) | `zstd` (`zstd-sys`) | BSD-3-Clause (upstream offers BSD-3-Clause or GPL-2.0; used under BSD-3-Clause) |

Rust crate dependencies keep their own licenses as recorded in `Cargo.lock`
and their crate metadata. No outside source is copied into this repository.
