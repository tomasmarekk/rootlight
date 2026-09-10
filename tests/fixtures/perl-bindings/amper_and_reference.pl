# Ampersand calls and code references share lexical callable lookup.
# Taking a reference is not itself a call; invoking the reference is separate.
use strict;
use warnings;
sub value { 13 }
{
    my sub value { 29 }
    print &value(), ',', (&value), ',', (\&value)->(), "\n";
}
print &value(), "\n";
