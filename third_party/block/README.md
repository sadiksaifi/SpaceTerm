# block

This is the `block` 0.1.6 source from crates.io, maintained at
https://github.com/SSheldon/rust-block. The native test helper comes from upstream
commit `642ea4a4a5853a21b55b05c34832a5f1bb1af61c`.

SpaceTerm declares `_NSConcreteStackBlock` as `c_void` and takes its raw address.
The [Apple Blocks ABI](https://clang.llvm.org/docs/Block-ABI-Apple.html) uses that
symbol's address for the `isa` pointer. Bare `extern` declarations now name the
`C` ABI explicitly. An uninhabited enum and a reference to it
are invalid here. This patch removes Rust's future-incompatibility warning from
the macOS dependencies that still use this crate.

The MIT license text is from upstream pull request #14, commit
`0c123b7be1e5e5fb14d561352a461c909b931baf`.
