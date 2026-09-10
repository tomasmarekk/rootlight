# Without strict subs, an undeclared bareword is a literal at its compile position.
# A later declaration and package switches do not retroactively turn it into a call.
my $before = value;
sub value { 13 }
my $after = value;
package Other;
my $other = value;
package main;
print $before, ',', $after, ',', $other, ',', value, "\n";
