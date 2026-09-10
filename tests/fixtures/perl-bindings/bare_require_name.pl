# A literal require name is a module name even beside a predeclared callable.
# A prefilled local INC entry keeps the finite oracle independent of the filesystem.
use strict;
use warnings;
BEGIN { $INC{'value.pm'} = 1; }
sub value { die "unexpected callable invocation"; }
require value;
print "loaded\n";
