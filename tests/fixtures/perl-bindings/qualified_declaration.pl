# Qualified declarations select their written package independently of ambient scope.
# The following call must find that same callable storage.
use strict;
use warnings;
package Harbor;
sub Cove::value { 29 }
print Cove::value(), "\n";
