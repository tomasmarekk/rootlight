# Complete C local-address original

Complete unchanged `dev/test/test_outlier_detector.c` from
[karpathy/llm.c](https://github.com/karpathy/llm.c), local original revision
`f1e2ace651495b74ae22d45d1723443fd00ecd3a`. The retained physical source is
1,778 bytes with 52 CRLFs and SHA-256
`463ee0862bd600db995853fc76eeb72237466747559ba7638bec6afcaeec9f94`.
The complete supplied 1,093-byte MIT license is retained in `LICENSE`, SHA-256
`3cdff5c93621ea940d99d493dd8b3eba9516f64b724aa3938e64b42bb32fed3f`.
The revision records custody; it does not assert that these physical CRLF bytes
are identical to the upstream Git blob. No authorship or relicensing is claimed.

The original writes `main` at 256..260 and the ordinary local `detector` at
290..298. The required address Reference at 320..328 belongs to that local;
main directly contains the local and the reverse Contains edge is absent.
Taking an address does not prove an initialized value Read. The external type,
included header and `init_detector`/`update_detector` call targets remain
unqualified. The complete original is the regression input; no synthesized
terminator, source normalization or substitute snippet supplies its proof.
