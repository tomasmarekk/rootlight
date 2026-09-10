# Existing lexical reads survive an our declaration's entire statement.
# The next statement sees the package alias, then the outer pad is restored.
use strict;
use warnings;
package Harbor;
my $value = 2;
{
    my @seen = ($value, our $value = $value + 1, $value);
    print join(',', @seen), "\n";
    print "$value\n";
}
print "$value,$Harbor::value\n";
