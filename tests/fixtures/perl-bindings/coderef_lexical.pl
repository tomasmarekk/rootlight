# A lexical CODE reference keeps its target across direct and ampersand invocation.
# The variable name is not the target function name.
use strict;
use warnings;
sub value { 13 }
my $call = \&value;
print $call->(), ',', &$call(), "\n";
