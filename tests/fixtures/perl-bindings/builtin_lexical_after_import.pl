# A later lexical function can shadow an imported package builtin override.
# Calls after the block return to the imported package function.
use strict;
use warnings;
package Harbor;
use subs 'length';
sub length { 29 }
{
    my sub length { 41 }
    print length('abc'), "\n";
}
print length('abc'), "\n";
