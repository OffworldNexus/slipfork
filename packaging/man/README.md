# Manpages

`slipfork.1` is generated — never edited by hand.

```console
$ cargo build -p slipfork
$ target/debug/slipfork --manpage > packaging/man/slipfork.1
```

`scripts/build-bundle.sh` runs that command against the freshly built release binary before it
gzips the page into the package bundle, and the `cli::manpage` unit test fails if the checked-in
page drifts from the `clap` tree. `slipfork-lsp.1` is maintained by hand for the separate
`slipfork-lsp` binary.
