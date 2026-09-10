# Evaluated substitution can mutate lexical CODE storage through runtime source text.
# The mutation's target spelling occurs only in a string, not as a direct scalar use.
use strict;
use warnings;
sub first { 13 }
sub second { 29 }
my $call = \&first;
my $program = '$call = \&second';
my $text = 'x';
$text =~ s/x/eval $program/e;
print $call->(), "\n";
