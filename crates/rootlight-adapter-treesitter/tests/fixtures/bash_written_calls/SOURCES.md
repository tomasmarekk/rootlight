# Written Bash wrapper source

This directory retains the complete original `nvm.sh` and its upstream MIT license
from the local independently inventoried `12_Bash_nvm` corpus checkout. The fixture
has no removed bodies, replacement names, normalized line endings or missing EOF.
It tests authored source dependencies without executing shell commands.

- Upstream repository: `nvm-sh/nvm`.
- Observed checkout revision: `65ded65d46c16481dbbe8e93f8ba9e6b35f20740`.
- Original source: `nvm.sh`, 168666 bytes, SHA-256
  `4f104e291bf720c981f609276f60be3157e6d7d125736e892230755c585e1e5c`.
- Retained license: `LICENSE.md`, copied byte for byte from that checkout.

The complete original source supplies literal wrapper calls at `[732,749)` and
`[779,808)`. The independent corpus contract expects `nvm_err` to depend on
`nvm_echo` and `nvm_err_with_colors` to depend on `nvm_echo_with_colors`. Those
expectations are unchanged by this fixture. Tests also use independently renamed
small source controls and reject dynamic commands, competing declarations and
nonordinary wrapper bodies.
