# Predeclared names resolve as calls while autoquoted keys remain literal.
# Parenthesized arithmetic and an assignment prevent bare calls absorbing print arguments.
use strict;
use warnings;
use feature 'signatures';
sub value { 13 }
my @values = (value, (value), (value) + 1);
print join(',', @values), "\n";
{
    my sub value { 29 }
    print value, ',', (value), ',', (value) + 1, "\n";
}
my %values = (value => 7);
print $values{value}, "\n";
sub increment ($number) { $number + 1 }
my $result = increment 3;
print $result, "\n";
