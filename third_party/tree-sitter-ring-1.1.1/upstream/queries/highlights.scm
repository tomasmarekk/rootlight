; Ring language syntax highlighting for Tree-sitter
; Compatible with: Neovim, Helix, Zed
;
; IMPORTANT: In tree-sitter highlights, LAST match wins for the same node.
; So generic rules come FIRST, specific rules come AFTER to override.

; ====================
; Generic Fallback (MUST be first so specific rules can override)
; ====================
(identifier) @variable

; ====================
; Comments
; ====================
(comment) @comment

; ====================
; Literals
; ====================
(number) @constant.numeric
(string) @string
(boolean) @constant.builtin.boolean
(null) @constant.builtin

; Interpolation opener (#{ inside a string)
(interpolation "#{") @punctuation.special

; Symbols (:name)
(symbol) @string.special.symbol

; ====================
; Variables & Identifiers (more specific than generic @variable)
; ====================
(self) @variable.builtin
(super) @variable.builtin

; Global variables ($var)
((identifier) @variable.builtin
  (#match? @variable.builtin "^\\$"))

; Instance variables (@var)
((identifier) @variable.member
  (#match? @variable.member "^@"))

; Ring builtin global variables (case-insensitive)
((identifier) @variable.builtin
  (#match? @variable.builtin "(?i)^(nl|tab|cr|stdin|stdout|stderr|sysargv|ccatcherror|ringoptionalfunctions)$"))

; ====================
; Functions
; ====================

; Function definitions
(function_definition
  name: (identifier) @function)

; Generic function calls
(call_expression
  (identifier) @function.call)

; Typed parameter type names
(typed_parameter
  type: (identifier) @type)

; Builtin function calls (case-insensitive) - comes AFTER generic to override
((call_expression
  (identifier) @function.builtin)
  (#match? @function.builtin "(?i)^(acos|add|addattribute|adddays|addmethod|ascii|asin|assert|atan|atan2|attributes|binarysearch|bytes2double|bytes2float|bytes2int|callgarbagecollector|callgc|ceil|cfunctions|char|chdir|checkoverflow|classes|classname|clearerr|clock|clockspersecond|closelib|copy|cos|cosh|currentdir|date|dec|del|diffdays|dir|direxists|double2bytes|eval|exefilename|exefolder|exp|fabs|fclose|feof|ferror|fexists|fflush|fgetc|fgetpos|fgets|filename|find|float2bytes|floor|fopen|fputc|fputs|fread|freopen|fseek|fsetpos|ftell|functions|fwrite|getarch|getattribute|getchar|getfilesize|getnumber|getpointer|getptr|getstring|globals|hex|hex2str|importpackage|input|insert|int2bytes|intvalue|isalnum|isalpha|isandroid|isattribute|iscfunction|isclass|iscntrl|isdigit|isfreebsd|isfunction|isglobal|isgraph|islinux|islist|islocal|islower|ismacosx|ismethod|ismsdos|isnull|isnumber|isobject|ispackage|ispackageclass|ispointer|isprint|isprivateattribute|isprivatemethod|ispunct|isspace|isstring|isunix|isupper|iswindows|iswindows64|isxdigit|left|len|lines|list|list2str|loadlib|locals|log|log10|lower|max|memcpy|memorycopy|mergemethods|methods|min|murmur3hash|newlist|nofprocessors|nothing|nullpointer|nullptr|number|obj2ptr|object2pointer|objectid|optionalfunc|packageclasses|packagename|packages|parentclassname|perror|pointer2object|pointer2string|pointercompare|pow|prevfilename|print|print2str|ptr2obj|ptr2str|ptrcmp|puts|raise|random|randomize|read|ref|refcount|reference|remove|rename|reverse|rewind|right|ring_state_delete|ring_state_findvar|ring_state_init|ring_state_main|ring_state_mainfile|ring_state_new|ring_state_newvar|ring_state_runcode|ring_state_runfile|ring_state_runobjectfile|ring_state_setvar|ringvm_callfunc|ringvm_calllist|ringvm_cfunctionslist|ringvm_classeslist|ringvm_codelist|ringvm_evalinscope|ringvm_fileslist|ringvm_functionslist|ringvm_genarray|ringvm_give|ringvm_hideerrormsg|ringvm_info|ringvm_ismempool|ringvm_memorylist|ringvm_packageslist|ringvm_passerror|ringvm_ringolists|ringvm_runcode|ringvm_scopescount|ringvm_see|ringvm_settrace|ringvm_tracedata|ringvm_traceevent|ringvm_tracefunc|ringvm_writeringo|setattribute|setpointer|setptr|shutdown|sin|sinh|sort|space|sqrt|srandom|str2hex|str2hexcstyle|str2list|strcmp|string|substr|swap|sysget|sysset|syssleep|system|sysunset|tan|tanh|tempfile|tempname|time|timelist|trim|type|ungetc|unsigned|upper|uptime|variablepointer|varptr|version|windowsnl|write|decimals|getpathtype|ring_give|ring_see|ring_state_filetokens|ring_state_resume|ring_state_runcodeatins|ring_state_scannererror|ring_state_stringtokens|ringvm_translatecfunction)$"))

; ====================
; Classes
; ====================
(class_definition
  name: (identifier) @type.definition)

(class_parent
  (qualified_identifier
    (identifier) @type))

; ====================
; Packages/Namespaces
; ====================
(package_definition
  name: (qualified_identifier
    (identifier) @module))

(import_statement
  (qualified_identifier
    (identifier) @module))

(load_statement
  (string) @string.special.path)

; ====================
; Object Creation
; ====================
(new_expression
  (qualified_identifier
    (identifier) @type))

; new X(...) chains (constructor args present)
(new_with_parens
  (qualified_identifier
    (identifier) @type))

; ====================
; Member Access
; ====================
(member_expression
  (identifier) @variable.member .)
; Method calls (specific: comes AFTER Member Access so it overrides)
(call_expression
  (member_expression
    (identifier) @function.method.call .))

; ====================
; Parameters
; ====================
(param_list
  (identifier) @variable.parameter)

(for_statement
  variable: (identifier) @variable.parameter)

; ====================
; Operators
; ====================
[
  "+" "-" "*" "/" "%" "**" "^^"
  "&" "|" "~" "^"
  "="
  "!=" "<" ">" "<=" ">="
  "<<" ">>"
  "&&" "||"
  "++" "--"
  "+=" "-=" "*=" "/=" "%="
  "&=" "|=" "^="
  "**=" "^^="
  "<<=" ">>="
  "!" "?"
] @operator

; ====================
; Punctuation
; ====================
["(" ")"] @punctuation.bracket
["[" "]"] @punctuation.bracket
["{" "}"] @punctuation.bracket

["," "."] @punctuation.delimiter
":" @punctuation.delimiter

; ====================
; Keywords
; ====================
[
  "if" "else" "elseif" "but" "ok" "endif"
  "switch" "on" "case" "other" "off" "endswitch"
  "while" "end" "endwhile"
  "for" "foreach" "to" "in" "step" "next" "endfor"
  "do" "again"
  "try" "catch" "done" "endtry"
] @keyword.control

[
  "func" "function" "def"
  "endfunc" "endfunction"
  "return" "call"
] @keyword.function

[
  "class" "endclass"
  "from" "private" "new"
] @keyword.type

[
  "package" "endpackage"
  "import" "load"
] @keyword.import

["see" "put" "give" "get"] @keyword

["exit" "break" "loop" "continue" "bye"] @keyword.control.return

["and" "or" "not"] @keyword.operator

; ====================
; Scanner Commands
; ====================
(scanner_command) @keyword.directive
