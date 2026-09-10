# A compile-time subs import can override a builtin in its package.
# Explicit CORE calls and qualified calls retain different targets.
use strict;
use warnings;
package North;
use subs 'length';
sub length { 29 }
print length('abc'), ',', CORE::length('abc'), "\n";
package South;
print length('abc'), ',', North::length('abc'), "\n";
