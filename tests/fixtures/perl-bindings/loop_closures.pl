# Each loop iteration gives captured lexical values their own binding instance.
# The surrounding variable is not the target of reads inside these closures.
use strict;
use warnings;
my $value = 9;
my @readers;
for my $value (1, 2) {
    push @readers, sub { return $value; };
}
print join(',', map { $_->() } @readers), "\n";
print "$value\n";
