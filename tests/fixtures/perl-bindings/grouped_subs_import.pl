# Parentheses preserve a literal argument list for the subs pragma.
# Both a unary builtin and a list operator select their package override.
use strict;
use warnings;
package Harbor;
use subs ('length', 'reverse');
sub length { 29 }
sub reverse { 41 }
print length('abc'), ',', scalar(reverse('abc')), "\n";
