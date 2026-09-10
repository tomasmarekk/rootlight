# Lexical builtin-name shadowing is checked independently of package imports.
# Leaving the lexical block must restore the ordinary builtin interpretation.
use strict;
use warnings;
package Harbor;
{
    my sub length { 41 }
    print length('abc'), "\n";
}
print length('abc'), "\n";
