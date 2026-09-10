# A qw import is a list of literal callable names, unlike one quoted argument.
# The imported package override must replace the builtin call target.
use strict;
use warnings;
package Harbor;
use subs qw(length);
sub length { 29 }
print length('abc'), "\n";
