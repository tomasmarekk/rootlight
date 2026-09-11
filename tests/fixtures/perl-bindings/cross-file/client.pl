# Explicit package calls and a local namesake have different callable storage.
# Runtime output provides independent truth for a later cross-file graph regression.
use strict;
use warnings;
use Measure ();

sub adjust { return $_[0] + 19; }
sub consume { return Measure::adjust(5); }
print consume(), ',', adjust(5), "\n";
