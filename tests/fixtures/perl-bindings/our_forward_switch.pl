# A visible our sub alias may redirect a later unqualified definition.
# The source's ambient package and the alias's storage must stay distinct.
use strict;
use warnings;
package North;
sub value { 13 }
package South;
sub value;
{
    our sub value;
    package North;
    sub value { 29 }
    print value(), "\n";
}
print North::value(), ',', South::value(), "\n";
