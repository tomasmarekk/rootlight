# A lexical package alias keeps its declaration package across later switches.
# The block form restores the enclosing package without retargeting the alias.
use strict;
use warnings;
package Harbor;
our $value = 5;
package Meadow;
$Meadow::value = 9;
print "$value,$Meadow::value\n";
package Cove {
    print __PACKAGE__, ":$value\n";
}
print __PACKAGE__, ":$value\n";
