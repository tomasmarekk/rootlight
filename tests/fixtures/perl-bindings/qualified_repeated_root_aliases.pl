# Repeated explicit root qualifiers still address the top-level package stash.
# Non-root components are retained and must not be stripped during normalization.
use strict;
use warnings;
package Cove;
our $value = 13;
sub value { 29 }
package Cove::main;
sub value { 41 }
package main;
print $main::main::Cove::value, ',', main::main::Cove::value(), ',', ::main::Cove::value(), ',', Cove::main::value(), "\n";
