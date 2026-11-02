# Original C formal-return fixture

`sieve.c` is copied byte-for-byte from the audited corpus checkout's
`65_WebAssembly_waforth/src/web/benchmarks/sieve/sieve.c`, project
[remko/waforth](https://github.com/remko/waforth), commit `3b337010bfc81ae8fc5168040b3887406d2c2e19`.
Its 508 physical bytes preserve CRLF and have SHA-256 `d3476eb7505e8ccf99b49227cdac4ef42c0323cdc3ba8618ec0c25eebd0d5b69`.
The working file is Git-clean. Removing CR from CRLF reproduces upstream Git blob
`cea6b4060982afb3abe99aaec7fd38cfc06885ef` exactly: 481 LF bytes, SHA-256 `0daf8adc923d6c3e821dfd31cef4417cf95728e56019f1aa29f3232bdd4eb0ff`.
The fixture directory's existing `-text` rule preserves the tested physical bytes.

`SIEVE-LICENSE` retains the complete upstream `LICENSE.txt`; its line-ending-normalized
content matches that same commit. The retained 1092 bytes have SHA-256
`d9e407508768f94211dd1902e43132faace94e4a226ea243e30992dfcbb93301`. These are third-party MIT-licensed sources;
no original authorship is claimed by Rootlight.

The selected native read is the `n` in the early `return n;` at physical bytes 89..90.
The regression requires its exact formal target and function-owned `Reads` relation;
later loops and declarations do not acquire additional binding authority.
