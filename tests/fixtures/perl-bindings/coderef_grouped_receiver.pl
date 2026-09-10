# Parentheses around a lexical CODE receiver preserve the same referenced callable.
# Result assignment avoids print's own argument-grouping precedence.
use strict;
use warnings;
sub value { 13 }
my $call = \&value;
my $first = ($call)->();
my $second = (( # transparent receiver group
    $call
))->();
print $first, ',', $second, "\n";
