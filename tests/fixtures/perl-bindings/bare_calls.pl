# Compile-time callable declarations disambiguate bare names without parentheses.
# Lexical shadowing applies after the inner declaration and ends with its scope.
use strict;
use warnings;
sub value { 13 }
print value, "\n";
{
    my sub value { 29 }
    print value, "\n";
}
