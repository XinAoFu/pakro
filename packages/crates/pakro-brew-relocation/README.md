# pakro-brew-relocation

Homebrew bottle relocation used by pakro. This crate rewrites placeholder paths
in text files, Mach-O load commands, and Linux ELF linkage while pouring
bottles. The caller supplies the Homebrew prefix and repository paths and
handles any macOS codesigning required after relocation.

This is an internal component of pakro; its API may change between releases.
