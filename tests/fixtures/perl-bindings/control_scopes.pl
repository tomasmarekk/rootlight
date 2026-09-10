# Loop and conditional declarations own their control expression and body.
# Exiting the whole construct restores the surrounding lexical binding.
use strict;
use warnings;
my $value = 40;
for my $value (1, 2) {
    print "$value\n";
}
print "$value\n";
if (my $value = 3) {
    print "$value\n";
} else {
    print "$value\n";
}
print "$value\n";
