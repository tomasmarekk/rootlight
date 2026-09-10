# Equal lexical variable spellings in nested pads retain distinct CODE values.
# Leaving the inner block restores the outer binding, not its most recent spelling.
use strict;
use warnings;
sub first { 13 }
sub second { 29 }
my $call = \&first;
{
    my $call = \&second;
    print $call->(), ',';
}
print $call->(), "\n";
