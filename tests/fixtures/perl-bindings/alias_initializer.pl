# An existing our alias wins throughout a new alias's declaration statement.
# Package switches affect the new storage, not earlier lexical aliases.
use strict;
use warnings;
package Harbor;
our $value = 2;
package Cove;
{
    my @seen = ($value, our $value = $value + 1, $value);
    print join(',', @seen), "\n";
    print "$value\n";
}
print "$value,$Cove::value\n";
