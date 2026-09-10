# Same-spelled variables in distinct packages have distinct identities.
# Qualified access does not follow a lexical alias selected in another package.
use strict;
use warnings;
package Harbor;
our $value = 10;
package Cove {
    our $value = 20;
    print "$value,$Harbor::value\n";
    print(\$value == \$Cove::value ? "same\n" : "different\n");
    print(\$Harbor::value != \$Cove::value ? "different\n" : "same\n");
}
print "$value,$Cove::value\n";
