# Bare invocants follow compile-time callable visibility before arrow dispatch.
# Different method results distinguish a literal class from a returned class name.
use strict;
use warnings;
package value;
sub method { 41 }
package Selected;
sub method { 29 }
package main;
my $before = value->method();
sub value { 'Selected' }
my $after = value->method();
print $before, ',', $after, ',', value(), "\n";
