# A previously undeclared our alias is available in its own initializer.
# No prior lexical binding exists to preserve within this statement.
use strict;
use warnings;
our $value = ($value // 0) + 1;
print "$value,$main::value\n";
