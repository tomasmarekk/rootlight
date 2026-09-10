# A lexical sub is visible after its declaration, not inside its own body.
# The body's unqualified call still selects the earlier package subroutine.
use strict;
use warnings;
package North;
sub value { 13 }
{
    print value(), "\n";
    my sub value { value() + 1 }
    print value(), "\n";
}
print value(), "\n";
