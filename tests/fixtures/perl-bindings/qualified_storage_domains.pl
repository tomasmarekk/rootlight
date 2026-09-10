# Qualified declarations create distinct callable slots without package statements.
# An empty qualification selects main rather than the surrounding package.
use strict;
use warnings;
package Harbor;
sub Cove::value { 13 }
sub Reef::value { 29 }
sub ::value { 41 }
print Cove::value(), ',', Reef::value(), ',', ::value(), ',', main::value(), "\n";
