# An explicitly qualified definition fills the prior package declaration.
# Its body still compiles in the surrounding package, not the written target package.
use strict;
use warnings;
package Cove;
sub value;
package Harbor;
sub value { 13 }
sub Cove::value { value() + 1 }
print Cove::value(), "\n";
package Cove;
print value(), "\n";
