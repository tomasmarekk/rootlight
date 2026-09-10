# Unqualified written function calls select the lexical package at that site.
# Qualified calls remain anchored independently of the current package.
use strict;
use warnings;
package Harbor;
sub value { return 10; }
print value(), "\n";
package Cove;
sub value { return 20; }
print value(), "\n";
print Harbor::value(), "\n";
package Meadow { print Cove::value(), "\n"; }
