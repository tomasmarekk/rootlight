# Complete C variable-scope original

Complete unchanged `test/should_run/xorshift64.c` from [https://github.com/minoki/LunarML](https://github.com/minoki/LunarML),
local original revision `ec4e3b3673be4abdaf37962ed6b01c1e36300ac9`. The exact305-byte source SHA256 is
`d203cdbead5250459bcfdc653ecb12393fa4eaa622bcd8a9d6b5455ea9d4258c`.
The full supplied MIT license is retained in `LICENSE`; no authorship or
relicensing is claimed. Corpus origin/revision is custody metadata, not a
claim that uninspected repository files equal their Git revision.

The source writes global `seed`, functions `xorshift64` and `main`, and the
for-initializer local `i`. Required value operands are seed115..119,
seed140..144, seed171..175 and i228..229. Global seed54..58 and local i221..222
must have exact Variable Definitions, and main must directly contain i.
The already-passing same-file main-to-xorshift64 dependency remains required.
The external printf target/header macro remains applicable and pending;
no guessed header declaration is added to these controls.
