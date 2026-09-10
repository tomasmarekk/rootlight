# Copying a CODE value does not make later assignments alias the original variable.
# Source-ordered flow must distinguish the two invocation targets after reassignment.
use strict;
use warnings;
sub first { 13 }
sub second { 29 }
my $call = \&first;
my $copy = $call;
$call = \&second;
print $copy->(), ',', $call->(), "\n";
