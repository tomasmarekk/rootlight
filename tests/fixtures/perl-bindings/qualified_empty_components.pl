# An empty non-root package component is not another alias of the root stash.
# Catch the two missing functions so the authored oracle has bounded successful output.
use strict;
use warnings;
package Cove;
sub value { 29 }
package main;
my $first = eval { ::::Cove::value() };
my $second = eval { main::::Cove::value() };
print Cove::value(), ',', (defined $first ? 'found' : 'missing'), ',', (defined $second ? 'found' : 'missing'), "\n";
