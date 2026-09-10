# Authored bindings exercise lexical scope, attribute paths and interpolated data.
# Syntax qualification never evaluates imports, paths or package expressions.
{ lib, system ? "portable", ... }@args:
let
  choose = value: if value != null then value else system;
  nested = rec { first = 1; second = first + 1; };
in {
  inherit system;
  inherit (nested) first second;
  package.name = "demo";
  package.run = name: "${name}-${system}";
  "quoted.key" = args.system or system;
  "${choose "dynamic"}" = true;
  path = ./modules/${system}/default.nix;
  search = <nixpkgs/lib>;
  home = ~/config.nix;
  values = [ (choose null) nested.second ];
  script = ''
    # phantom = 42; is string content, not a binding.
    echo ''${literal} ${system} λ😀
  '';
  enabled = with lib; assert nested ? first; true;
}
