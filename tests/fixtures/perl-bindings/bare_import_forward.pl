# A compile-time subs import proves a bare callable before its later body.
# This is distinct from retroactively treating an earlier undeclared term as a call.
use strict;
use warnings;
use subs ('value');
my $result = value;
sub value { 29 }
print $result, "\n";
