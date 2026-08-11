# Rootlight lru patch

This directory contains the crates.io `lru` 0.16.4 source required by
Tantivy 0.26.1.

Rootlight backports the panic-safety fix for `RUSTSEC-2026-0253` from upstream
commit `2776ded569ee89a99c515bca8194f65639182c96` (lru-rs pull request
238). The patch detaches a removed node before dropping its key, so a panicking
`Drop` implementation cannot leave dangling pointers in the cache list. The
upstream regression test is included with the backport.

Remove this patch directory and the root `[patch.crates-io]` entry once a
released Tantivy version accepts `lru` 0.18.2 or newer.
