# The body of a new our sub may still bind a prior lexical callable.
# A bounded counter prevents an incorrect recursive interpretation from hanging.
use strict;
use warnings;
package South;
our $calls = 0;
{
    my sub value { 29 }
    {
        our sub value {
            return -100 if ++$calls > 2;
            value() + 1;
        }
        print value(), "\n";
    }
    print value(), "\n";
}
print South::value(), "\n";
