# Bash declaration and exact-source fixture; this file is never executed.
ROOT="🌍"
declare -a ITEMS=(one two)
function emit() {
  local message="$1"
  local pending
  printf '%s\n' "$message"
}
process() {
  for item in "${ITEMS[@]}"; do
    emit "$item"
  done
}
render() {
  cat <<ONE | cat <<'終端'
first $ROOT
ONE
second $literal
終端
}
process
