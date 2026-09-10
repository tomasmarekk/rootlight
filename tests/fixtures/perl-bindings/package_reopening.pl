# Reopening one package does not allocate a new package-variable identity.
# Separate lexical aliases can denote that same storage from different blocks.
use strict;
use warnings;
package Harbor;
our $value = 10;
package Cove {
    package Harbor;
    our $value;
    print(\$value == \$Harbor::value ? "same\n" : "different\n");
    $value = 12;
}
print "$value\n";
