# Explicit main and leading separators address the same top-level package stash.
# Package switches, variable reads and callable declarations must agree on that identity.
use strict;
use warnings;
package Cove;
our $value = 13;
sub value { 29 }
package main::Cove;
print $value, ',', value(), ',', ::Cove::value(), "\n";
package main;
print $Cove::value, ',', $main::Cove::value, ',', $::Cove::value, "\n";
sub main::Cove::other;
sub ::Cove::other { 41 }
print Cove::other(), ',', main::Cove::other(), ',', ::Cove::other(), "\n";
