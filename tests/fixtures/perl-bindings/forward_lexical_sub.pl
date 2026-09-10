# A later plain sub definition fills a visible lexical forward declaration.
# The same-spelled package function retains its independent implementation.
use strict;
use warnings;
package North;
sub value { 13 }
{
    my sub value;
    sub value { 29 }
    print value(), "\n";
}
print North::value(), "\n";
