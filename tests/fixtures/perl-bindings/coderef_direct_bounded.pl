# Direct CODE invocation is evaluated before print to avoid Perl list-operator ambiguity.
# One and two transparent grouping wrappers preserve the same written callable target.
use strict;
use warnings;
sub value { 13 }
my $first = (\&value)->();
my $second = ((\&value))->();
print $first, ',', $second, "\n";
