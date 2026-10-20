class Demo { static function f() { var raw = "outer"; { var raw = "inner"; trace(raw); } trace(raw); } }
