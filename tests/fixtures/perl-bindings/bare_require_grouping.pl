# Parentheses select expression evaluation instead of the direct bare module syntax.
# Prefilled INC entries isolate compile-position behavior from module loading.
use strict;
use warnings;
BEGIN { $INC{'value.pm'} = 1; $INC{'other.pm'} = 1; }
sub value { print "called\n"; 'other.pm' }
require (value);
require ((value));
print "done\n";
