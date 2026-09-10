# Distinguishes a new lexical declaration from its initializer's outer read.
# This compiler oracle is independent of Rootlight's structural extraction.
use strict;
use warnings;
my $value = 11;
{
    my $value = $value + 1;
    print "$value\n";
}
print "$value\n";
