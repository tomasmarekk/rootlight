class Demo { static function f() {
#if target
var raw = "a";
#else
var raw = "b";
#end
trace(raw);
} }
