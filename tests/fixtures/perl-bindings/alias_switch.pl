# Alias identity is the package selected at declaration, not at each read.
# Reference comparisons test identity rather than coincidentally equal values.
use strict;
use warnings;
package Harbor;
our $value = 7;
package Cove;
print(\$value == \$Harbor::value ? "same\n" : "different\n");
package Meadow {
    print(\$value == \$Harbor::value ? "same\n" : "different\n");
}
print "$value\n";
