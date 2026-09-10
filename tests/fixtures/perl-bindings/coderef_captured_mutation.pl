# One invocation source can reach different callables across captured-variable updates.
# A lexical binding alone therefore cannot prove a single static call target.
use strict;
use warnings;
sub first { 13 }
sub second { 29 }
my $call = \&first;
sub invoke { $call->() }
print invoke(), ',';
$call = \&second;
print invoke(), "\n";
