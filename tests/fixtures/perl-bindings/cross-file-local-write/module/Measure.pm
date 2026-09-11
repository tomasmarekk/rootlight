# A package-owned callable used by an authored two-file binding reference.
# The caller supplies the module search root explicitly; no corpus code is loaded.
package Measure;
use strict;
use warnings;

sub adjust { return $_[0] + 7; }
1;
