# Builtin list operators have a distinct native grammar form from unary builtins.
# A proven package import still selects the authored override.
use strict;
use warnings;
package Harbor;
use subs 'reverse';
sub reverse { 29 }
print scalar reverse('abc'), "\n";
