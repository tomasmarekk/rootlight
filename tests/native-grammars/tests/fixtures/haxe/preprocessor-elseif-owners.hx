class Demo {
#if debug
public static function first() {}
#elseif macro
public static function second() {}
#else
public static function fallback() {}
#end
}
