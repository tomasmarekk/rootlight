# An our sub alias selects its declaration package across later package switches.
# Inner alias scope restores a prior lexical function without changing either package.
use strict;
use warnings;
package South;
sub value { 41 }
package North;
sub value { 13 }
{
    my sub value { 29 }
    print value(), "\n";
    {
        our sub value;
        package South;
        print value(), ',', South::value(), "\n";
    }
    print value(), "\n";
}
print North::value(), "\n";
