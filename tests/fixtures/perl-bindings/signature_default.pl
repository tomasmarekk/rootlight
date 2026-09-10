# Signature defaults can read preceding parameters of the same invocation.
# This boundary is different from ordinary my-declaration statements.
use v5.36;
sub adjust ($value, $step = $value + 1) {
    return "$value,$step";
}
print adjust(4), "\n";
