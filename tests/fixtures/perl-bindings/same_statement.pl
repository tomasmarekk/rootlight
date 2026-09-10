# Reads in a declaration statement still select the previous lexical binding.
# The next statement observes the newly introduced binding instead.
use strict;
use warnings;
my $value = 2;
{
    my @seen = ($value, my $value = $value + 1, $value);
    print join(',', @seen), "\n";
    print "$value\n";
}
