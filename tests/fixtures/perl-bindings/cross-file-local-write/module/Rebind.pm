# A compile-time import can replace storage declared in a different file.
# The client imports this only after declaring its local namesake.
package Rebind;
use strict;
use warnings;
no warnings 'redefine';
*main::adjust = sub { return $_[0] + 31; };
1;
