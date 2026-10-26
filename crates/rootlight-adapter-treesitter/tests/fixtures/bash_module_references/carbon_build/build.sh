#!bin/bash

CARBON_COMPILER="carbon"
CARBON_INSTALL_DIR="/usr/local"
BIN_DIR="bin"

mkdir -p "$BIN_DIR"

for src_file in *.carbon; do
	exe_file="$(basename "$src_file" .carbon)"

	"$CARBON_COMPILER" compile --output="$BIN_DIR/$exe_file".o "$src_file"
	"$CARBON_COMPILER" link --output="$BIN_DIR/$exe_file" "$BIN_DIR/$exe_file".o "$CARBON_INSTALL_DIR"/lib/carbon/core/*.o "$CARBON_INSTALL_DIR"/lib/carbon/core/prelude/types/*.o

	echo "Compiled $src_file to $BIN_DIR/$exe_file"
done
